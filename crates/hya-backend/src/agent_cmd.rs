use std::collections::{BTreeSet, HashSet};
use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;
use serde::Serialize;

#[derive(Subcommand)]
pub(crate) enum AgentCommand {
    /// List available agents. By default prints only the built-in primary agent
    /// (Compat-parity output); pass `--all` to also list ordinary agents from
    /// the embedded built-in catalog.
    List {
        /// Also list ordinary agents from the embedded built-in catalog.
        #[arg(long)]
        all: bool,
    },
}

#[derive(Serialize)]
struct PermissionRule {
    permission: &'static str,
    pattern: &'static str,
    action: &'static str,
}

struct AgentInfo {
    name: &'static str,
    mode: &'static str,
    permission: Vec<PermissionRule>,
}

pub(crate) fn run(command: AgentCommand) -> anyhow::Result<()> {
    match command {
        AgentCommand::List { all } => list(all),
    }
}

fn list(all: bool) -> anyhow::Result<()> {
    let workdir = std::env::current_dir().context("resolve current directory")?;
    let text = list_text_for(&workdir, all)?;
    let mut out = std::io::stdout().lock();
    if let Err(error) = out.write_all(text.as_bytes()) {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(error).context("write agent list");
    }
    Ok(())
}

/// Render the agent list. Without `all`, prints only the built-in primary agent
/// with its default permission rules (Compat-parity output). With `all`, adds
/// ordinary agents from the build-embedded catalog only — never inspects
/// `.hya`/`.claude`/`.opencode`/config agent files. System agents
/// (compaction/title/summary) are excluded by ordinary `can_spawn` reachability,
/// not by role. `workdir` is unused for discovery; kept so tests can prove a
/// tracked disk agent is ignored.
fn list_text_for(_workdir: &std::path::Path, all: bool) -> anyhow::Result<String> {
    let mut natives = native_agents();
    natives.sort_by(|a, b| a.name.cmp(b.name));
    let native_names: HashSet<&str> = natives.iter().map(|agent| agent.name).collect();

    let mut text = String::new();
    for agent in &natives {
        text.push_str(agent.name);
        text.push_str(" (");
        text.push_str(agent.mode);
        text.push_str(")\n  ");
        text.push_str(&serde_json::to_string_pretty(&agent.permission)?);
        text.push('\n');
    }

    if !all {
        return Ok(text);
    }

    // Single embedded catalog authority — same Arc the runtime bootstrap uses.
    let catalog = hya_app::builtin_catalog()?;
    // Ordinary reachability: any catalog agent lists the id in can_spawn.
    let reachable: BTreeSet<&str> = catalog
        .bundles()
        .iter()
        .flat_map(|bundle| bundle.agents.iter())
        .flat_map(|agent| agent.can_spawn.iter().map(|id| id.as_str()))
        .collect();

    let mut ordinary: Vec<_> = catalog
        .bundles()
        .iter()
        .flat_map(|bundle| bundle.agents.iter())
        .filter(|agent| {
            let name = agent.stable_id.as_str();
            !native_names.contains(name) && reachable.contains(name)
        })
        .collect();
    ordinary.sort_by(|a, b| a.stable_id.as_str().cmp(b.stable_id.as_str()));

    for agent in ordinary {
        text.push_str(agent.stable_id.as_str());
        text.push_str(" (");
        text.push_str(agent.role.selector_mode());
        text.push_str(")\n");
        if let Some(description) = agent.description.as_deref().filter(|d| !d.is_empty()) {
            text.push_str("  ");
            text.push_str(description);
            text.push('\n');
        }
    }
    Ok(text)
}

fn native_agents() -> Vec<AgentInfo> {
    vec![AgentInfo {
        name: "build",
        mode: "primary",
        permission: vec![
            PermissionRule {
                permission: "read",
                pattern: "*",
                action: "allow",
            },
            PermissionRule {
                permission: "glob",
                pattern: "*",
                action: "allow",
            },
            PermissionRule {
                permission: "grep",
                pattern: "*",
                action: "allow",
            },
        ],
    }]
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Parse top-level agent name lines (`name (mode)`), ignoring indented body.
    fn listed_agent_names(text: &str) -> Vec<String> {
        text.lines()
            .filter(|line| !line.starts_with(char::is_whitespace))
            .filter_map(|line| {
                let (name, rest) = line.split_once(" (")?;
                rest.contains(')').then(|| name.to_string())
            })
            .collect()
    }

    #[test]
    fn list_all_uses_catalog_ordinary_agents_not_disk() {
        // Disk agent must never appear: list is catalog-only.
        let dir = std::env::temp_dir().join(format!("hya-agent-list-{}", std::process::id()));
        let agents = dir.join(".claude/agents");
        std::fs::create_dir_all(&agents).expect("create .claude/agents");
        std::fs::write(
            agents.join("tester.md"),
            "---\nname: tester\ndescription: a probe agent\ncategory: quick\n---\nProbe body.\n",
        )
        .expect("write agent file");

        let all = list_text_for(&dir, true).expect("render agent list --all");
        let default = list_text_for(&dir, false).expect("render agent list");
        std::fs::remove_dir_all(&dir).ok();

        let all_names = listed_agent_names(&all);
        let default_names = listed_agent_names(&default);

        // Default remains the narrow built-in build row with permission display.
        assert_eq!(
            default_names,
            vec!["build".to_string()],
            "default must list only build:\n{default}"
        );
        assert!(
            default.contains("\"permission\": \"read\""),
            "default build row must keep permission display:\n{default}"
        );
        assert!(
            !default.contains("tester"),
            "default must not list disk agents:\n{default}"
        );
        assert!(
            !default_names
                .iter()
                .any(|n| n == "general" || n == "hya-main"),
            "default must not expand catalog ordinary agents:\n{default}"
        );

        // --all never inspects disk agent files.
        assert!(
            !all_names.iter().any(|n| n == "tester"),
            "tracked disk agent must not appear even with --all:\n{all}"
        );
        assert!(
            !all.contains("category: quick"),
            "disk category must not surface under --all:\n{all}"
        );

        // --all adds ordinary catalog agents (can_spawn reachability), not role filter.
        assert!(
            all_names.iter().any(|n| n == "build"),
            "native build agent should list:\n{all}"
        );
        assert!(
            all_names.iter().any(|n| n == "general"),
            "--all must include catalog ordinary agent general:\n{all}"
        );
        assert!(
            all_names.iter().any(|n| n == "hya-main"),
            "--all must include catalog ordinary agent hya-main:\n{all}"
        );
        // System compaction/title/summary are not ordinarily reachable.
        for system in ["compaction", "title", "summary"] {
            assert!(
                !all_names.iter().any(|n| n == system),
                "--all must exclude system agent {system} (can_spawn, not role):\n{all}"
            );
        }
    }
}
