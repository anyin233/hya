use std::path::PathBuf;

use hya_core::AgentSpec;
use hya_proto::{ModelRef, SessionId};
use serde_json::Value;

use crate::ServerState;

use super::agent_catalog::AgentEntry;

pub(super) fn apply_agent_entry(
    agent: &mut AgentSpec,
    entry: &AgentEntry,
    active_model: &ModelRef,
    config: &Value,
) {
    if let Some(reasoning) = super::reasoning_options::resolve_reasoning(
        entry.variant.as_deref(),
        &entry.options,
        active_model,
        config,
    ) {
        agent.reasoning = Some(reasoning);
    }
}

pub(in crate::opencode) async fn agent_with_guidance(st: &ServerState) -> AgentSpec {
    let mut agent = (*st.agent).clone();
    let workdir = super::location::workdir(st);
    let config = super::reasoning_options::load_opencode_config(&workdir);
    if let Some(entry) = super::agent_catalog::list(&workdir, st)
        .into_iter()
        .find(|entry| entry.name.as_str() == agent.name.as_str())
    {
        let active_model = agent.model.clone();
        apply_agent_entry(&mut agent, &entry, &active_model, &config);
    }
    if let Some(guidance) = guidance(st).await {
        agent.system_prompt = format!("{}\n\n{}", agent.system_prompt.trim_end(), guidance);
    }
    agent
}

// Run a turn under the session's switched agent, not the server default (the
// engine already resolves the model per session; this overrides agent identity).
pub(in crate::opencode) async fn session_agent_with_guidance(
    st: &ServerState,
    session: SessionId,
) -> AgentSpec {
    let mut agent = (*st.agent).clone();
    let workdir = super::location::workdir(st);
    let config = super::reasoning_options::load_opencode_config(&workdir);
    if let Ok(projection) = st.engine.store().read_projection(session).await {
        let active_name = projection
            .session
            .agent
            .clone()
            .unwrap_or_else(|| agent.name.clone());
        let active_model = projection
            .session
            .model
            .clone()
            .unwrap_or_else(|| agent.model.clone());
        if let Some(entry) = super::agent_catalog::list(&workdir, st)
            .into_iter()
            .find(|entry| entry.name.as_str() == active_name.as_str())
        {
            if projection
                .session
                .agent
                .as_ref()
                .is_some_and(|name| name.as_str() != agent.name.as_str())
            {
                if let Some(prompt) = entry.prompt.clone() {
                    agent.system_prompt = prompt;
                }
                agent.name = active_name;
            }
            apply_agent_entry(&mut agent, &entry, &active_model, &config);
        }
    }
    if let Some(guidance) = guidance(st).await {
        agent.system_prompt = format!("{}\n\n{}", agent.system_prompt.trim_end(), guidance);
    }
    agent
}

pub(in crate::opencode) async fn list(st: &ServerState) -> Vec<Value> {
    let config = st.global.config().await;
    let Some(entries) = super::reference_entries::reference_entries(&config) else {
        return Vec::new();
    };
    let base = super::location::workdir(st);
    let references = entries
        .iter()
        .filter_map(|(name, entry)| {
            super::reference_entries::valid_alias(name)
                .then(|| super::reference_entries::reference(name, entry, &base))
                .flatten()
        })
        .collect::<Vec<_>>();
    super::reference_entries::materialize_git(&references);
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
