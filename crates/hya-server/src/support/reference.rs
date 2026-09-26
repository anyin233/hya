use std::path::{Path, PathBuf};

use hya_proto::AgentName;
use std::sync::Arc;

use hya_core::{
    AgentSpec, CoreError, PromptEnv, discover_context_files, render_environment_and_context, today,
};
use hya_proto::SessionId;
use serde_json::Value;

use crate::ServerState;

/// Request-scoped turn agent: Harness/session identity without guidance overlay.
///
/// Guidance is pre-rendered separately and passed into core's one composition
/// seam; it must not live on [`AgentSpec`].
pub(crate) struct SessionTurnAgent {
    pub agent: AgentSpec,
    /// Immutable pre-rendered reference guidance for this turn, if any.
    pub guidance: Option<Arc<str>>,
}

/// A session's recorded workdir. Every session has one; the server has no
/// working directory to fall back on (ADR-0024).
pub(crate) async fn session_workdir(
    st: &ServerState,
    session: SessionId,
) -> Result<PathBuf, CoreError> {
    st.engine
        .store()
        .read_projection(session)
        .await?
        .session
        .workdir
        .map(PathBuf::from)
        .ok_or_else(|| CoreError::Invalid(format!("session not found: {session}")))
}

// Run a turn under the session's switched agent, not the server default (the
// engine already resolves the model per session; this overrides agent identity).
//
// Pre-renders reference guidance once for the turn; does not put it on AgentSpec.
pub(crate) async fn session_agent_with_guidance(
    st: &ServerState,
    session: SessionId,
) -> Result<SessionTurnAgent, CoreError> {
    let projection = st.engine.store().read_projection(session).await?;
    let workdir = projection
        .session
        .workdir
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| CoreError::Invalid(format!("session not found: {session}")))?;
    let mut agent = (*st.agent).clone();
    agent.workdir = workdir.clone();
    let active_name = projection
        .session
        .agent
        .clone()
        .unwrap_or_else(|| agent.name.clone());
    agent.name = active_name;
    // Session model is applied by the engine from projection; AgentSpec.model
    // remains the server default fallback for routes that read it before bind.
    let active_model = projection
        .session
        .model
        .clone()
        .unwrap_or_else(|| agent.model.clone());
    agent.model = active_model;
    let guidance = guidance_at(st, &workdir).await;
    Ok(SessionTurnAgent { agent, guidance })
}

pub(crate) async fn list_at(st: &ServerState, workdir: &Path) -> Vec<Value> {
    let config = st.global.config().await;
    let Some(entries) = crate::support::reference_entries::reference_entries(&config) else {
        return Vec::new();
    };
    let references = entries
        .iter()
        .filter_map(|(name, entry)| {
            crate::support::reference_entries::valid_alias(name)
                .then(|| crate::support::reference_entries::reference(name, entry, workdir))
                .flatten()
        })
        .collect::<Vec<_>>();
    crate::support::reference_entries::materialize_git(&references);
    references
}

pub(crate) async fn external_directories_at(st: &ServerState, workdir: &Path) -> Vec<PathBuf> {
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

/// Pre-render request-scoped Harness project guidance + described references.
///
/// Discovers current workdir `AGENTS.md` once (parent→child), renders Environment
/// and project-context separators via core prompt helpers, then appends sorted
/// reference guidance. Result is immutable for the turn — core receives only
/// this text, never paths/parser/raw files.
async fn guidance_at(st: &ServerState, workdir: &Path) -> Option<Arc<str>> {
    if st.pure_guidance {
        // `--pure`: no external AGENTS/context or reference discovery at all.
        return None;
    }
    let env = PromptEnv {
        cwd: workdir.to_string_lossy().into_owned(),
        platform: std::env::consts::OS.to_string(),
        date: today(),
    };
    let context = discover_context_files(workdir);
    let project = render_environment_and_context(&env, &context);
    let references = render_reference_guidance(st, workdir).await;

    let text = match references {
        Some(refs) if !project.is_empty() => format!("{project}\n\n{refs}"),
        Some(refs) => refs,
        None if !project.is_empty() => project,
        None => return None,
    };
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(Arc::<str>::from(text))
    }
}

/// Sorted described project references (prior in-process shape and separators).
async fn render_reference_guidance(st: &ServerState, workdir: &Path) -> Option<String> {
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

/// Resolve the Agent metadata for a synthetic shell turn and persist any
/// client model override.
///
/// Re-homed from the retired legacy command routes; drives shell message
/// attribution for `/v1` shell turns.
pub(crate) async fn shell_agent(
    st: &crate::ServerState,
    session: SessionId,
    req: &hya_proto::api::ShellRequest,
) -> Result<hya_core::AgentSpec, crate::ApiError> {
    if let Some(model) = req.model_ref() {
        st.engine.switch_model(session, model).await?;
    }
    let mut turn = session_agent_with_guidance(st, session).await?;
    if let Some(agent) = req
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|agent| !agent.is_empty())
    {
        turn.agent.name = AgentName::new(agent);
    }
    Ok(turn.agent)
}
