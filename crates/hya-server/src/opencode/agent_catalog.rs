use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::Path;

use serde_json::Value;

use super::agent_permission::PermissionRule;
use super::agent_sources::AgentChange;
use super::json_merge::merge_json_map;

pub(super) struct AgentEntry {
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) mode: String,
    pub(super) hidden: bool,
    pub(super) native: bool,
    pub(super) model: Option<String>,
    pub(super) variant: Option<String>,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) color: Option<String>,
    pub(super) steps: Option<NonZeroU64>,
    pub(super) options: BTreeMap<String, Value>,
    pub(super) request_headers: BTreeMap<String, String>,
    pub(super) request_body: BTreeMap<String, Value>,
    pub(super) permissions: Vec<PermissionRule>,
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

pub(super) fn list(workdir: &Path, st: &crate::ServerState) -> Vec<AgentEntry> {
    let configured = configured_default(workdir, st.default_agent.as_deref());
    let mut agents = merged_entries(workdir, st.include_global_agents);
    super::agent_defaults::sort(&mut agents, configured.as_deref());
    agents
}

pub(super) fn default_name(workdir: &Path, st: &crate::ServerState) -> Option<String> {
    let configured = configured_default(workdir, st.default_agent.as_deref());
    let agents = merged_entries(workdir, st.include_global_agents);
    super::agent_defaults::selected_name(&agents, configured.as_deref())
}

/// The configured default agent: a workdir `opencode.json` `default_agent` wins, otherwise the
/// server config-file default (`~/.config/hya/config.yaml`). `None` falls back to `build`.
fn configured_default(workdir: &Path, config_default: Option<&str>) -> Option<String> {
    super::agent_sources::default_agent(workdir).or_else(|| config_default.map(str::to_owned))
}

fn merged_entries(workdir: &Path, include_global: bool) -> Vec<AgentEntry> {
    let mut agents = native_entries();
    let global_permissions = super::agent_sources::global_permissions(workdir);
    for agent in &mut agents {
        agent.permissions.extend(global_permissions.clone());
    }
    if include_global {
        for change in super::agent_disk_sources::global_disk_agents() {
            apply_change(&mut agents, change, &global_permissions);
        }
    }
    for change in super::agent_sources::config_agents(workdir) {
        apply_change(&mut agents, change, &global_permissions);
    }
    for change in super::agent_disk_sources::disk_agents(workdir) {
        apply_change(&mut agents, change, &global_permissions);
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
            variant: None,
            temperature: None,
            top_p: None,
            color: None,
            steps: None,
            options: BTreeMap::new(),
            request_headers: BTreeMap::new(),
            request_body: BTreeMap::new(),
            permissions: Vec::new(),
            prompt: super::agent_native_prompts::get(agent.name).map(str::to_string),
        })
        .collect()
}

fn apply_change(
    agents: &mut Vec<AgentEntry>,
    change: AgentChange,
    global_permissions: &[PermissionRule],
) {
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
        if let Some(variant) = change.variant {
            existing.variant = Some(variant);
        }
        if let Some(temperature) = change.temperature {
            existing.temperature = Some(temperature);
        }
        if let Some(top_p) = change.top_p {
            existing.top_p = Some(top_p);
        }
        if let Some(color) = change.color {
            existing.color = Some(color);
        }
        if let Some(steps) = change.steps {
            existing.steps = Some(steps);
        }
        if let Some(options) = change.options {
            merge_json_map(&mut existing.options, options);
        }
        if let Some(headers) = change.request_headers {
            existing.request_headers.extend(headers);
        }
        if let Some(body) = change.request_body {
            existing.request_body.extend(body);
        }
        if let Some(permissions) = change.permissions {
            existing.permissions.extend(permissions);
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
            variant: change.variant,
            temperature: change.temperature,
            top_p: change.top_p,
            color: change.color,
            steps: change.steps,
            options: change.options.unwrap_or_default(),
            request_headers: change.request_headers.unwrap_or_default(),
            request_body: change.request_body.unwrap_or_default(),
            permissions: {
                let mut permissions = global_permissions.to_vec();
                permissions.extend(change.permissions.unwrap_or_default());
                permissions
            },
            prompt: change.prompt,
        });
    }
}
