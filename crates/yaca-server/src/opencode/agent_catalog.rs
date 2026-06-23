use std::path::Path;

use super::agent_sources::AgentChange;

pub(super) struct AgentEntry {
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) mode: String,
    pub(super) hidden: bool,
    pub(super) native: bool,
    pub(super) model: Option<String>,
    pub(super) prompt: Option<String>,
}

struct NativeAgent {
    name: &'static str,
    description: Option<&'static str>,
    mode: &'static str,
    hidden: bool,
}

const NATIVE_AGENTS: &[NativeAgent] = &[
    NativeAgent {
        name: "build",
        description: Some("The default agent. Executes tools based on configured permissions."),
        mode: "primary",
        hidden: false,
    },
    NativeAgent {
        name: "plan",
        description: Some("Plan mode. Disallows all edit tools."),
        mode: "primary",
        hidden: false,
    },
    NativeAgent {
        name: "general",
        description: Some(
            "General-purpose agent for researching complex questions and executing multi-step tasks. Use this agent to execute multiple units of work in parallel.",
        ),
        mode: "subagent",
        hidden: false,
    },
    NativeAgent {
        name: "explore",
        description: Some(
            "Fast agent specialized for exploring codebases. Use this when you need to quickly find files by patterns (eg. \"src/components/**/*.tsx\"), search code for keywords (eg. \"API endpoints\"), or answer questions about the codebase (eg. \"how do API endpoints work?\"). When calling this agent, specify the desired thoroughness level: \"quick\" for basic searches, \"medium\" for moderate exploration, or \"very thorough\" for comprehensive analysis across multiple locations and naming conventions.",
        ),
        mode: "subagent",
        hidden: false,
    },
    NativeAgent {
        name: "compaction",
        description: None,
        mode: "primary",
        hidden: true,
    },
    NativeAgent {
        name: "title",
        description: None,
        mode: "primary",
        hidden: true,
    },
    NativeAgent {
        name: "summary",
        description: None,
        mode: "primary",
        hidden: true,
    },
];

pub(super) fn list(workdir: &Path) -> Vec<AgentEntry> {
    let mut agents = native_entries();
    for change in super::agent_sources::disk_agents(workdir) {
        apply_change(&mut agents, change);
    }
    agents
}

fn native_entries() -> Vec<AgentEntry> {
    NATIVE_AGENTS
        .iter()
        .map(|agent| AgentEntry {
            name: agent.name.to_string(),
            description: agent.description.map(str::to_string),
            mode: agent.mode.to_string(),
            hidden: agent.hidden,
            native: true,
            model: None,
            prompt: None,
        })
        .collect()
}

fn apply_change(agents: &mut Vec<AgentEntry>, change: AgentChange) {
    if change.remove {
        agents.retain(|agent| agent.name != change.name);
        return;
    }
    if let Some(existing) = agents.iter_mut().find(|agent| agent.name == change.name) {
        if let Some(description) = change.description {
            existing.description = Some(description);
        }
        if let Some(mode) = change.mode {
            existing.mode = mode;
        }
        if let Some(hidden) = change.hidden {
            existing.hidden = hidden;
        }
        if let Some(model) = change.model {
            existing.model = Some(model);
        }
        if let Some(prompt) = change.prompt {
            existing.prompt = Some(prompt);
        }
    } else {
        agents.push(AgentEntry {
            name: change.name,
            description: change.description,
            mode: change.mode.unwrap_or_else(|| "all".to_string()),
            hidden: change.hidden.unwrap_or(false),
            native: false,
            model: change.model,
            prompt: change.prompt,
        });
    }
}
