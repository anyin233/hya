use std::path::{Path, PathBuf};

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
    if let Some(prompt) = &entry.prompt {
        agent.system_prompt = prompt.clone();
    }
    if let Some(reasoning) = super::reasoning_options::resolve_reasoning(
        entry.variant.as_deref(),
        &entry.options,
        active_model,
        config,
    ) {
        agent.reasoning = Some(reasoning);
    }
}

pub(in crate::compat) async fn agent_with_guidance(st: &ServerState) -> AgentSpec {
    let workdir = super::location::workdir(st);
    agent_with_guidance_at(st, &workdir).await
}

pub(in crate::compat) async fn agent_with_guidance_at(
    st: &ServerState,
    workdir: &Path,
) -> AgentSpec {
    let mut agent = (*st.agent).clone();
    agent.workdir = workdir.to_path_buf();
    let config = super::reasoning_options::load_compat_config(workdir);
    if let Some(entry) = super::agent_catalog::list(workdir, st)
        .into_iter()
        .find(|entry| entry.name.as_str() == agent.name.as_str())
    {
        let active_model = agent.model.clone();
        apply_agent_entry(&mut agent, &entry, &active_model, &config);
    }
    if let Some(guidance) = guidance_at(st, workdir).await {
        agent.system_prompt = format!("{}\n\n{}", agent.system_prompt.trim_end(), guidance);
    }
    agent
}

pub(in crate::compat) async fn session_workdir(st: &ServerState, session: SessionId) -> PathBuf {
    st.engine
        .store()
        .read_projection(session)
        .await
        .ok()
        .and_then(|projection| projection.session.workdir.map(PathBuf::from))
        .unwrap_or_else(|| super::location::workdir(st))
}

// Run a turn under the session's switched agent, not the server default (the
// engine already resolves the model per session; this overrides agent identity).
pub(in crate::compat) async fn session_agent_with_guidance(
    st: &ServerState,
    session: SessionId,
) -> AgentSpec {
    let Ok(projection) = st.engine.store().read_projection(session).await else {
        return agent_with_guidance(st).await;
    };
    let workdir = projection
        .session
        .workdir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| super::location::workdir(st));
    let mut agent = (*st.agent).clone();
    agent.workdir = workdir.clone();
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
    agent.name = active_name.clone();
    let config = super::reasoning_options::load_compat_config(&workdir);
    if let Some(entry) = super::agent_catalog::list(&workdir, st)
        .into_iter()
        .find(|entry| entry.name.as_str() == active_name.as_str())
    {
        apply_agent_entry(&mut agent, &entry, &active_model, &config);
    }
    if let Some(guidance) = guidance_at(st, &workdir).await {
        agent.system_prompt = format!("{}\n\n{}", agent.system_prompt.trim_end(), guidance);
    }
    agent
}

pub(in crate::compat) async fn list(st: &ServerState) -> Vec<Value> {
    let workdir = super::location::workdir(st);
    list_at(st, &workdir).await
}

pub(in crate::compat) async fn list_at(st: &ServerState, workdir: &Path) -> Vec<Value> {
    let config = st.global.config().await;
    let Some(entries) = super::reference_entries::reference_entries(&config) else {
        return Vec::new();
    };
    let references = entries
        .iter()
        .filter_map(|(name, entry)| {
            super::reference_entries::valid_alias(name)
                .then(|| super::reference_entries::reference(name, entry, workdir))
                .flatten()
        })
        .collect::<Vec<_>>();
    super::reference_entries::materialize_git(&references);
    references
}

pub(in crate::compat) async fn external_directories_at(
    st: &ServerState,
    workdir: &Path,
) -> Vec<PathBuf> {
    list_at(st, workdir)
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

async fn guidance_at(st: &ServerState, workdir: &Path) -> Option<String> {
    let mut references: Vec<_> = list_at(st, workdir)
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
