use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use yaca_core::AgentSpec;

use crate::ServerState;

pub(in crate::opencode) async fn agent_with_guidance(st: &ServerState) -> AgentSpec {
    let mut agent = (*st.agent).clone();
    if let Some(guidance) = guidance(st).await {
        agent.system_prompt = format!("{}\n\n{}", agent.system_prompt.trim_end(), guidance);
    }
    agent
}

pub(in crate::opencode) async fn list(st: &ServerState) -> Vec<Value> {
    let config = st.global.config().await;
    let Some(entries) = reference_entries(&config) else {
        return Vec::new();
    };
    let base = super::location::workdir(st);
    let references = entries
        .iter()
        .filter_map(|(name, entry)| {
            valid_alias(name)
                .then(|| reference(name, entry, &base))
                .flatten()
        })
        .collect::<Vec<_>>();
    materialize_git(&references);
    references
}

pub(in crate::opencode) async fn external_directories(st: &ServerState) -> Vec<PathBuf> {
    list(st)
        .await
        .into_iter()
        .filter_map(|reference| {
            reference
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
        .collect()
}

async fn guidance(st: &ServerState) -> Option<String> {
    let mut references: Vec<_> = list(st)
        .await
        .into_iter()
        .filter(|reference| {
            reference
                .get("description")
                .and_then(Value::as_str)
                .is_some()
        })
        .collect();
    references.sort_by_key(|reference| {
        reference
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    });

    let mut lines = vec![
        "Project references provide additional directories that can be accessed when relevant."
            .to_string(),
        "<available_references>".to_string(),
    ];
    for reference in references {
        let (Some(name), Some(path), Some(description)) = (
            reference.get("name").and_then(Value::as_str),
            reference.get("path").and_then(Value::as_str),
            reference.get("description").and_then(Value::as_str),
        ) else {
            continue;
        };
        lines.extend([
            "  <reference>".to_string(),
            format!("    <name>{name}</name>"),
            format!("    <path>{path}</path>"),
            format!("    <description>{description}</description>"),
            "  </reference>".to_string(),
        ]);
    }
    if lines.len() == 2 {
        return None;
    }
    lines.push("</available_references>".to_string());
    Some(lines.join("\n"))
}

fn reference_entries(config: &Value) -> Option<&Map<String, Value>> {
    config
        .get("references")
        .or_else(|| config.get("reference"))
        .and_then(Value::as_object)
}

fn reference(name: &str, entry: &Value, base: &Path) -> Option<Value> {
    match entry {
        Value::String(value) if is_local_shorthand(value) => {
            local_reference(name, value, None, None, base)
        }
        Value::String(value) => git_reference(name, value, None, None, None),
        Value::Object(object) if object.contains_key("path") => local_reference(
            name,
            object.get("path")?.as_str()?,
            object.get("description").and_then(Value::as_str),
            object.get("hidden").and_then(Value::as_bool),
            base,
        ),
        Value::Object(object) if object.contains_key("repository") => git_reference(
            name,
            object.get("repository")?.as_str()?,
            object.get("branch").and_then(Value::as_str),
            object.get("description").and_then(Value::as_str),
            object.get("hidden").and_then(Value::as_bool),
        ),
        _ => None,
    }
}

fn local_reference(
    name: &str,
    value: &str,
    description: Option<&str>,
    hidden: Option<bool>,
    base: &Path,
) -> Option<Value> {
    let path = resolve_path(base, value).to_string_lossy().into_owned();
    reference_value(
        name,
        &path,
        description,
        hidden,
        Map::from_iter([
            ("type".to_string(), json!("local")),
            ("path".to_string(), json!(path)),
        ]),
    )
}

fn git_reference(
    name: &str,
    repository: &str,
    branch: Option<&str>,
    description: Option<&str>,
    hidden: Option<bool>,
) -> Option<Value> {
    if let Some(branch) = branch
        && !super::reference_repository::valid_branch(branch)
    {
        return None;
    }
    let parsed = super::reference_repository::parse(repository)?;
    let path = super::reference_repository::cache_path(&parsed)
        .to_string_lossy()
        .into_owned();
    let mut source = Map::from_iter([
        ("type".to_string(), json!("git")),
        ("repository".to_string(), json!(repository)),
    ]);
    source.insert(
        "branch".to_string(),
        branch.map_or(Value::Null, |branch| json!(branch)),
    );
    reference_value(name, &path, description, hidden, source)
}

fn materialize_git(references: &[Value]) {
    for reference in references {
        let (Some(path), Some(source)) = (
            reference.get("path").and_then(Value::as_str),
            reference.get("source").and_then(Value::as_object),
        ) else {
            continue;
        };
        if source.get("type").and_then(Value::as_str) != Some("git") {
            continue;
        }
        let Some(repository) = source.get("repository").and_then(Value::as_str) else {
            continue;
        };
        super::reference_cache::materialize(
            repository,
            source.get("branch").and_then(Value::as_str),
            PathBuf::from(path),
        );
    }
}

fn reference_value(
    name: &str,
    path: &str,
    description: Option<&str>,
    hidden: Option<bool>,
    mut source: Map<String, Value>,
) -> Option<Value> {
    let mut out = Map::from_iter([
        ("name".to_string(), json!(name)),
        ("path".to_string(), json!(path)),
    ]);
    let description = description.map_or(Value::Null, |description| json!(description));
    let hidden = hidden.map_or(Value::Null, |hidden| json!(hidden));
    source.insert("description".to_string(), description.clone());
    source.insert("hidden".to_string(), hidden.clone());
    out.insert("description".to_string(), description);
    out.insert("hidden".to_string(), hidden);
    out.insert("source".to_string(), Value::Object(source));
    Some(Value::Object(out))
}

fn valid_alias(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|ch| ch == '/' || ch.is_whitespace() || ch == '`' || ch == ',')
}

fn is_local_shorthand(value: &str) -> bool {
    value.starts_with('.') || value.starts_with('/') || value.starts_with('~')
}

fn resolve_path(base: &Path, value: &str) -> PathBuf {
    let path = if let Some(path) = value.strip_prefix("~/") {
        home_dir().unwrap_or_else(|| PathBuf::from("~")).join(path)
    } else {
        PathBuf::from(value)
    };
    let path = if path.is_absolute() {
        path
    } else {
        base.join(path)
    };
    std::fs::canonicalize(&path).unwrap_or(path)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
