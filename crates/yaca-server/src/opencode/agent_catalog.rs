use std::path::{Path, PathBuf};

use serde::Deserialize;

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

#[derive(Default, Deserialize)]
struct AgentFrontmatter {
    description: Option<String>,
    mode: Option<String>,
    hidden: Option<bool>,
    model: Option<String>,
    disable: Option<bool>,
    disabled: Option<bool>,
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
    for change in disk_agents(workdir) {
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

struct AgentChange {
    name: String,
    description: Option<String>,
    mode: Option<String>,
    hidden: Option<bool>,
    model: Option<String>,
    prompt: Option<String>,
    remove: bool,
}

fn disk_agents(workdir: &Path) -> Vec<AgentChange> {
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
