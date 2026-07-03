use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::agent_options::{AgentOptions, from_config as agent_options};
use super::agent_permission::PermissionRule;
use super::agent_sources::AgentChange;

type RequestBody = BTreeMap<String, Value>;
type RequestHeaders = BTreeMap<String, String>;

#[derive(Default, Deserialize)]
struct AgentFrontmatter {
    name: Option<String>,
    description: Option<String>,
    mode: Option<String>,
    hidden: Option<bool>,
    model: Option<String>,
    category: Option<String>,
    resident: Option<bool>,
    variant: Option<String>,
    temperature: Option<f64>,
    top_p: Option<f64>,
    color: Option<String>,
    steps: Option<NonZeroU64>,
    #[serde(rename = "maxSteps")]
    max_steps: Option<NonZeroU64>,
    options: Option<AgentOptions>,
    request: Option<InlineRequest>,
    readonly: Option<bool>,
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

struct AgentFile {
    name: String,
    path: PathBuf,
    primary: bool,
}

pub(super) fn disk_agents(workdir: &Path) -> Vec<AgentChange> {
    let mut files = Vec::new();
    for root in [
        workdir.join(".opencode/agent"),
        workdir.join(".opencode/agents"),
        // Shared with Claude Code (mirrors how skills read `.claude/skills`), plus
        // the hya-native project dir. Same frontmatter schema as `.opencode/agent`.
        workdir.join(".claude/agents"),
        workdir.join(".hya/agents"),
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

/// Agents from the user's global config dirs (`~/.config/hya/agents`, plus the Compat
/// `~/.config/opencode/agent` location for superset parity). Mode comes from each file's
/// frontmatter; workdir agents are applied afterwards so a project can still override these.
pub(super) fn global_disk_agents() -> Vec<AgentChange> {
    let mut files = Vec::new();
    for root in global_agent_dirs() {
        collect_markdown_files(&root, &root, false, &mut files);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files.into_iter().filter_map(disk_agent).collect()
}

fn global_agent_dirs() -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME")
        && !dir.is_empty()
    {
        bases.push(PathBuf::from(dir));
    }
    if let Ok(home) = std::env::var("HOME") {
        let xdg_default = PathBuf::from(home).join(".config");
        if !bases.contains(&xdg_default) {
            bases.push(xdg_default);
        }
    }
    let mut dirs: Vec<PathBuf> = bases
        .iter()
        .flat_map(|base| {
            [
                base.join("hya/agents"),
                base.join("hya/agent"),
                base.join("compat/agent"),
                base.join("compat/agents"),
            ]
        })
        .collect();
    // Shared with Claude Code: `~/.claude/agents` (home-based, not XDG), mirroring
    // how skills read `~/.claude/skills`.
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        dirs.push(PathBuf::from(home).join(".claude/agents"));
    }
    dirs
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
    let steps = frontmatter.steps.or(frontmatter.max_steps);
    let (request_headers, request_body) = request_parts(frontmatter.request);
    Some(AgentChange {
        name: frontmatter.name.unwrap_or(file.name),
        description: frontmatter.description,
        mode,
        hidden: frontmatter.hidden,
        model: frontmatter.model,
        category: frontmatter.category,
        resident: frontmatter.resident,
        variant: frontmatter.variant,
        temperature: frontmatter.temperature,
        top_p: frontmatter.top_p,
        color: frontmatter.color,
        steps,
        options: agent_options(frontmatter.options, frontmatter.extra),
        request_headers,
        request_body,
        permissions: readonly_permissions(frontmatter.readonly),
        prompt: Some(prompt),
        remove: frontmatter.disable.unwrap_or(false) || frontmatter.disabled.unwrap_or(false),
    })
}

fn readonly_permissions(readonly: Option<bool>) -> Option<Vec<PermissionRule>> {
    // ponytail: `readonly` is sugar for a deny-edit rule; absence emits no rule so the agent
    // inherits the main agent's permission plane rather than getting an empty (deny-all) set.
    readonly.unwrap_or(false).then(|| {
        vec![PermissionRule::new(
            "edit".to_string(),
            "*".to_string(),
            "deny".to_string(),
        )]
    })
}

fn request_parts(request: Option<InlineRequest>) -> (Option<RequestHeaders>, Option<RequestBody>) {
    let Some(request) = request else {
        return (None, None);
    };
    (request.headers, request.body)
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
