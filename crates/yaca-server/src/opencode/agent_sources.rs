use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::agent_options::{AgentOptions, from_config as agent_options};
use super::agent_permission::PermissionRule;
use super::agent_permission_config::{
    ConfigPermissionRule, LegacyPermissions, LegacyTools, rules as permission_rules,
};

type RequestBody = BTreeMap<String, Value>;
type RequestHeaders = BTreeMap<String, String>;

pub(super) struct AgentChange {
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) mode: Option<String>,
    pub(super) hidden: Option<bool>,
    pub(super) model: Option<String>,
    pub(super) variant: Option<String>,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) color: Option<String>,
    pub(super) steps: Option<NonZeroU64>,
    pub(super) options: Option<AgentOptions>,
    pub(super) request_headers: Option<RequestHeaders>,
    pub(super) request_body: Option<RequestBody>,
    pub(super) permissions: Option<Vec<PermissionRule>>,
    pub(super) prompt: Option<String>,
    pub(super) remove: bool,
}

#[derive(Default, Deserialize)]
struct AgentConfig {
    default_agent: Option<String>,
    permissions: Option<Vec<ConfigPermissionRule>>,
    permission: Option<LegacyPermissions>,
    tools: Option<LegacyTools>,
    agent: Option<BTreeMap<String, InlineAgent>>,
    agents: Option<BTreeMap<String, InlineAgent>>,
    mode: Option<BTreeMap<String, InlineAgent>>,
    modes: Option<BTreeMap<String, InlineAgent>>,
}

#[derive(Default, Deserialize)]
struct InlineAgent {
    description: Option<String>,
    mode: Option<String>,
    hidden: Option<bool>,
    model: Option<String>,
    variant: Option<String>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    color: Option<String>,
    steps: Option<NonZeroU64>,
    #[serde(rename = "maxSteps")]
    max_steps: Option<NonZeroU64>,
    options: Option<AgentOptions>,
    request: Option<InlineRequest>,
    permission: Option<LegacyPermissions>,
    permissions: Option<Vec<ConfigPermissionRule>>,
    tools: Option<LegacyTools>,
    prompt: Option<String>,
    system: Option<String>,
    disable: Option<bool>,
    disabled: Option<bool>,
    #[serde(flatten)]
    extra: AgentOptions,
}

#[derive(Default, Deserialize)]
struct InlineRequest {
    headers: Option<RequestHeaders>,
    body: Option<RequestBody>,
}

pub(super) fn config_agents(workdir: &Path) -> Vec<AgentChange> {
    let mut agents = Vec::new();
    for path in config_paths(workdir) {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some(config) = parse_config(&content) else {
            continue;
        };
        append_inline_agents(config.agent, false, &mut agents);
        append_inline_agents(config.agents, false, &mut agents);
        append_inline_agents(config.mode, true, &mut agents);
        append_inline_agents(config.modes, true, &mut agents);
    }
    agents
}

pub(super) fn default_agent(workdir: &Path) -> Option<String> {
    let mut default_agent = None;
    for path in config_paths(workdir) {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some(config) = parse_config(&content) else {
            continue;
        };
        if config.default_agent.is_some() {
            default_agent = config.default_agent;
        }
    }
    default_agent
}

pub(super) fn global_permissions(workdir: &Path) -> Vec<PermissionRule> {
    let mut rules = Vec::new();
    for path in config_paths(workdir) {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let Some(config) = parse_config(&content) else {
            continue;
        };
        if let Some(config_rules) =
            permission_rules(config.permissions, config.permission, config.tools)
        {
            rules.extend(config_rules);
        }
    }
    rules
}

fn config_paths(workdir: &Path) -> [PathBuf; 4] {
    [
        workdir.join("opencode.json"),
        workdir.join("opencode.jsonc"),
        workdir.join(".opencode/opencode.json"),
        workdir.join(".opencode/opencode.jsonc"),
    ]
}

fn parse_config(content: &str) -> Option<AgentConfig> {
    super::jsonc::from_str(content).ok()
}

fn append_inline_agents(
    map: Option<BTreeMap<String, InlineAgent>>,
    primary: bool,
    agents: &mut Vec<AgentChange>,
) {
    for (name, agent) in map.unwrap_or_default() {
        let (request_headers, request_body) = request_parts(agent.request);
        let steps = agent.steps.or(agent.max_steps);
        let mode = if primary {
            Some("primary".to_string())
        } else {
            agent.mode
        };
        agents.push(AgentChange {
            name,
            description: agent.description,
            mode,
            hidden: agent.hidden,
            model: agent.model,
            variant: agent.variant,
            temperature: agent.temperature,
            top_p: agent.top_p,
            color: agent.color,
            steps,
            options: agent_options(agent.options, agent.extra),
            request_headers,
            request_body,
            permissions: permission_rules(agent.permissions, agent.permission, agent.tools),
            prompt: agent.system.or(agent.prompt),
            remove: agent.disable.unwrap_or(false) || agent.disabled.unwrap_or(false),
        });
    }
}

fn request_parts(request: Option<InlineRequest>) -> (Option<RequestHeaders>, Option<RequestBody>) {
    let Some(request) = request else {
        return (None, None);
    };
    (request.headers, request.body)
}
