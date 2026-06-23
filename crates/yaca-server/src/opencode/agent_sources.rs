use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub(super) struct AgentChange {
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) mode: Option<String>,
    pub(super) hidden: Option<bool>,
    pub(super) model: Option<String>,
    pub(super) variant: Option<String>,
    pub(super) prompt: Option<String>,
    pub(super) remove: bool,
}

#[derive(Default, Deserialize)]
struct AgentFrontmatter {
    description: Option<String>,
    mode: Option<String>,
    hidden: Option<bool>,
    model: Option<String>,
    variant: Option<String>,
    disable: Option<bool>,
    disabled: Option<bool>,
}

#[derive(Default, Deserialize)]
struct AgentConfig {
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
    prompt: Option<String>,
    system: Option<String>,
    disable: Option<bool>,
    disabled: Option<bool>,
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

pub(super) fn disk_agents(workdir: &Path) -> Vec<AgentChange> {
    let mut files = Vec::new();
    for root in [
        workdir.join(".opencode/agent"),
        workdir.join(".opencode/agents"),
    ] {
        collect_markdown_files(&root, &root, false, &mut files);
    }
    for root in [
        workdir.join(".opencode/mode"),
        workdir.join(".opencode/modes"),
    ] {
        collect_markdown_files(&root, &root, true, &mut files);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files.into_iter().filter_map(disk_agent).collect()
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
            prompt: agent.system.or(agent.prompt),
            remove: agent.disable.unwrap_or(false) || agent.disabled.unwrap_or(false),
        });
    }
}

struct AgentFile {
    name: String,
    path: PathBuf,
    primary: bool,
}

fn collect_markdown_files(base: &Path, dir: &Path, primary: bool, files: &mut Vec<AgentFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !primary {
            collect_markdown_files(base, &path, primary, files);
        } else if path.extension().is_some_and(|extension| extension == "md")
            && let Some(name) = agent_name(base, &path)
        {
            files.push(AgentFile {
                name,
                path,
                primary,
            });
        }
    }
}

fn disk_agent(file: AgentFile) -> Option<AgentChange> {
    let content = std::fs::read_to_string(file.path).ok()?;
    let (frontmatter, prompt) = parse_agent_file(&content)?;
    let mode = if file.primary {
        Some("primary".to_string())
    } else {
        frontmatter.mode
    };
    Some(AgentChange {
        name: file.name,
        description: frontmatter.description,
        mode,
        hidden: frontmatter.hidden,
        model: frontmatter.model,
        variant: frontmatter.variant,
        prompt: Some(prompt),
        remove: frontmatter.disable.unwrap_or(false) || frontmatter.disabled.unwrap_or(false),
    })
}

fn agent_name(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let name = relative
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    name.strip_suffix(".md").map(str::to_string)
}

fn parse_agent_file(content: &str) -> Option<(AgentFrontmatter, String)> {
    let Some((frontmatter, body)) = split_frontmatter(content) else {
        return Some((AgentFrontmatter::default(), content.trim().to_string()));
    };
    let metadata = if frontmatter.trim().is_empty() {
        AgentFrontmatter::default()
    } else {
        serde_norway::from_str(frontmatter).ok()?
    };
    Some((metadata, body.trim().to_string()))
}

fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    let (frontmatter, body) = rest.split_once("\n---")?;
    Some((
        frontmatter.strip_suffix('\r').unwrap_or(frontmatter),
        body.strip_prefix("\r\n")
            .or_else(|| body.strip_prefix('\n'))
            .unwrap_or(body),
    ))
}
