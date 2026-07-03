use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;
use serde::Serialize;

#[derive(Subcommand)]
pub(crate) enum AgentCommand {
    /// List available agents. By default prints only the built-in primary agent
    /// (Compat-parity output); pass `--all` to also list user-defined agents
    /// discovered on disk (`.claude/agents`, `.hya/agents`, `~/.config/hya/agents`, …).
    List {
        /// Also list user-defined agents discovered from disk.
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

/// Render the agent list for `workdir`. Without `all`, prints only the built-in
/// primary agent with its default permission rules (Compat-parity output). With
/// `all`, additionally lists every agent discovered on disk (`.claude/agents`,
/// `.hya/agents`, `~/.config/hya/agents`, `.opencode/agent`, …) — the same
/// catalog the model-facing `list_agents` tool and the subagent spawn path
/// resolve, so a user who drops a markdown agent file can confirm it is picked up.
fn list_text_for(workdir: &std::path::Path, all: bool) -> anyhow::Result<String> {
    let mut natives = native_agents();
    natives.sort_by(|a, b| a.name.cmp(b.name));
    let native_names: std::collections::HashSet<&str> =
        natives.iter().map(|agent| agent.name).collect();

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

    // Discovered agents (native non-hidden extras like plan/general/explore plus
    // any user-authored markdown agents). Skip names already printed above.
    let mut discovered = hya_server::agent_definitions(workdir, true);
    discovered.sort_by(|a, b| a.name.cmp(&b.name));
    for def in discovered {
        if native_names.contains(def.name.as_str()) {
            continue;
        }
        text.push_str(&def.name);
        text.push_str(" (");
        text.push_str(&def.mode);
        text.push(')');
        if let Some(category) = def.category.as_deref().filter(|c| !c.is_empty()) {
            text.push_str(" [category: ");
            text.push_str(category);
            text.push(']');
        }
        text.push('\n');
        if let Some(description) = def.description.as_deref().filter(|d| !d.is_empty()) {
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

    #[test]
    fn list_all_includes_disk_agents_but_default_stays_parity() {
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

        // --all surfaces the user-defined disk agent + its category.
        assert!(
            all.contains("build"),
            "native build agent should list:\n{all}"
        );
        assert!(
            all.contains("tester"),
            "disk .claude/agents agent should list:\n{all}"
        );
        assert!(
            all.contains("category: quick"),
            "category should surface:\n{all}"
        );

        // Default output stays the Compat-parity native shape (no disk agents).
        assert!(
            default.contains("build"),
            "default should list build:\n{default}"
        );
        assert!(
            !default.contains("tester"),
            "default (no --all) must not list disk agents (parity):\n{default}"
        );
    }
}
