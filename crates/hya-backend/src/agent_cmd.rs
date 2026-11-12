//! `hya agent list`: the built-in agent catalog (the compiled-in first-party
//! bundles plus installed bundles), without composing a runtime.

use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum AgentCommand {
    /// List the catalog's selectable agents (compiled-in and installed bundle
    /// agents). On-disk `.hya/`/`.claude/` agent files and config-declared
    /// agents are not part of that catalog.
    List,
}

pub(crate) fn run(command: AgentCommand) -> anyhow::Result<()> {
    match command {
        AgentCommand::List => list(),
    }
}

fn list() -> anyhow::Result<()> {
    let text = list_text()?;
    let mut out = std::io::stdout().lock();
    if let Err(error) = out.write_all(text.as_bytes()) {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(error).context("write agent list");
    }
    Ok(())
}

/// Render the catalog's selectable agents, one block each: `id (mode)` plus
/// its origin, then the description when it has one. Reserved system agents
/// (compaction/title/summary) are excluded because no ordinary agent reaches
/// them; neither is anything from disk.
fn list_text() -> anyhow::Result<String> {
    let catalog = hya_app::builtin_agent_catalog()?;
    let mut ordinary = catalog.selectable();
    ordinary.sort_by(|left, right| left.stable_id.cmp(right.stable_id));

    let mut text = String::new();
    for agent in ordinary {
        text.push_str(agent.stable_id);
        text.push_str(" (");
        text.push_str(agent.selector_mode());
        text.push(')');
        text.push_str(match agent.origin {
            hya_core::AgentOrigin::Builtin => " [builtin]\n",
            hya_core::AgentOrigin::Bundle { .. } => " [bundle]\n",
        });
        if let Some(description) = agent.description.filter(|text| !text.is_empty()) {
            text.push_str("  ");
            text.push_str(description);
            text.push('\n');
        }
    }
    Ok(text)
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
    fn list_shows_the_catalog_and_never_disk_agents() {
        // Disk agents must never appear: the listing is catalog-only.
        let dir = std::env::temp_dir().join(format!("hya-agent-list-{}", std::process::id()));
        let agents = dir.join(".claude/agents");
        std::fs::create_dir_all(&agents).expect("create .claude/agents");
        std::fs::write(
            agents.join("tester.md"),
            "---\nname: tester\ndescription: a probe agent\ncategory: quick\n---\nProbe body.\n",
        )
        .expect("write agent file");

        let text = list_text().expect("render agent list");
        std::fs::remove_dir_all(&dir).ok();
        let names = listed_agent_names(&text);

        assert!(
            names.iter().any(|name| name == "hya-main"),
            "the built-in primary agent must list:\n{text}"
        );
        assert!(
            names.iter().any(|name| name == "hya-task"),
            "catalog ordinary agents must list:\n{text}"
        );
        assert!(
            !names.iter().any(|name| name == "tester"),
            "tracked disk agent must not appear:\n{text}"
        );
        assert!(
            !text.contains("category: quick"),
            "disk metadata must not surface:\n{text}"
        );
        // System compaction/title/summary are not ordinarily reachable.
        for system in ["hya-compaction", "hya-title", "hya-summary"] {
            assert!(
                !names.iter().any(|name| name == system),
                "the listing must exclude system agent {system}:\n{text}"
            );
        }
    }
}
