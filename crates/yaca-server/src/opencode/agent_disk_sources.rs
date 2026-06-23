use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::agent_permission_config::{
    ConfigPermissionRule, LegacyPermissions, rules as permission_rules,
};
use super::agent_sources::AgentChange;

type RequestBody = BTreeMap<String, Value>;
type RequestHeaders = BTreeMap<String, String>;

#[derive(Default, Deserialize)]
struct AgentFrontmatter {
    description: Option<String>,
    mode: Option<String>,
    hidden: Option<bool>,
    model: Option<String>,
    variant: Option<String>,
    color: Option<String>,
    steps: Option<NonZeroU64>,
    #[serde(rename = "maxSteps")]
    max_steps: Option<NonZeroU64>,
    request: Option<InlineRequest>,
    permission: Option<LegacyPermissions>,
    permissions: Option<Vec<ConfigPermissionRule>>,
    disable: Option<bool>,
    disabled: Option<bool>,
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
        name: file.name,
        description: frontmatter.description,
        mode,
        hidden: frontmatter.hidden,
        model: frontmatter.model,
        variant: frontmatter.variant,
        color: frontmatter.color,
        steps,
        request_headers,
        request_body,
        permissions: permission_rules(frontmatter.permissions, frontmatter.permission),
        prompt: Some(prompt),
        remove: frontmatter.disable.unwrap_or(false) || frontmatter.disabled.unwrap_or(false),
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
