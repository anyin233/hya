use std::io::Write as _;

use anyhow::Context as _;
use clap::Subcommand;
use serde::Serialize;

#[derive(Subcommand)]
pub(crate) enum AgentCommand {
    /// List all available agents.
    List,
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

fn list_text() -> anyhow::Result<String> {
    let mut agents = native_agents();
    agents.sort_by(|a, b| a.name.cmp(b.name));
    let mut text = String::new();
    for agent in agents {
        text.push_str(agent.name);
        text.push_str(" (");
        text.push_str(agent.mode);
        text.push_str(")\n  ");
        text.push_str(&serde_json::to_string_pretty(&agent.permission)?);
        text.push('\n');
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
