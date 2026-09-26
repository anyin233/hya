//! Map the single bound BundleCatalog into catalog agent metadata rows.
//!
//! Not a second catalog authority: binds once per request workdir and projects
//! role → mode and can_spawn reachability → wire `hidden` from that catalog only.
//!
//! Also owns the sole `default_agent` fallback chain used by session create and
//! list sorting — `ServerState.default_agent`, then the bound process agent. No
//! agent definition merge and no external config reading.

use std::path::Path;

use axum::http::StatusCode;
use hya_bundle::BundleError;
use hya_core::{CoreError, TurnBinding};
use hya_proto::{AgentName, ModelRef};

use crate::{ApiError, ServerState};

/// Shared projection used by `/api/agent` and legacy `/agent`.
pub(crate) struct BoundAgentRow {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) mode: String,
    pub(crate) hidden: bool,
    pub(crate) color: Option<String>,
    pub(crate) prompt: Option<String>,
    pub(crate) model: Option<String>,
}

/// Capture one workdir `TurnBinding` and exact-resolve a root-session agent id.
///
/// When `requested` is present, that id is used. When omitted, the root default
/// is chosen in order: `ServerState.default_agent`, then `st.agent.name`. The
/// candidate is exact-resolved in that binding — no `general` fallback and no
/// role gate. Unknown ids surface as `BundleError::UnknownAgentId` via
/// `CoreError`/`ApiError`.
pub(crate) async fn resolve_session_agent(
    st: &ServerState,
    workdir: &Path,
    requested: Option<&str>,
) -> Result<AgentName, ApiError> {
    let binding = st.engine.bind_root_runtime(workdir).await?;
    resolve_agent_from_binding(st, &binding, requested)
}

/// Resolve a new root Session's Agent and model from one immutable binding.
pub(crate) async fn resolve_new_session_agent_model(
    st: &ServerState,
    workdir: &Path,
    requested_agent: Option<&str>,
    explicit_model: Option<ModelRef>,
) -> Result<(AgentName, ModelRef), ApiError> {
    let binding = st.engine.bind_root_runtime(workdir).await?;
    let agent = resolve_agent_from_binding(st, &binding, requested_agent)?;
    let model = resolve_session_model(st, binding, &agent, explicit_model).await?;
    Ok((agent, model))
}

/// Exact-resolve one root Agent against a previously captured binding.
fn resolve_agent_from_binding(
    st: &ServerState,
    binding: &TurnBinding,
    requested: Option<&str>,
) -> Result<AgentName, ApiError> {
    let candidate = match requested {
        Some(id) => id.to_string(),
        None => st
            .default_agent
            .clone()
            .unwrap_or_else(|| st.agent.name.as_str().to_string()),
    };
    let agent = binding.resolve_agent(&candidate).ok_or_else(|| {
        ApiError::from(CoreError::from(BundleError::UnknownAgentId {
            agent_id: candidate,
        }))
    })?;
    Ok(AgentName::new(agent.stable_id))
}

/// Resolve the model for a root Session from the same binding as its Agent.
///
/// An explicit request model remains authoritative and bypasses the Agent model
/// control. For an installed control, an omitted model must match the exact
/// resolved Agent row; a failed or incomplete list is an error rather than a
/// fallback, because using the process model could disagree with advertised
/// Agent state. The empty control keeps the prior process-base behavior.
async fn resolve_session_model(
    st: &ServerState,
    binding: TurnBinding,
    agent: &AgentName,
    explicit: Option<ModelRef>,
) -> Result<ModelRef, ApiError> {
    let Some(explicit) = explicit else {
        if !st.agent_model_control.available() {
            return Ok(st.agent.model.clone());
        }

        let states = st
            .agent_model_control
            .list(binding, st.agent.model.clone())
            .await
            .map_err(|error| {
                let error = crate::AgentModelControlError::new(error.code, error.message);
                ApiError::structured(StatusCode::SERVICE_UNAVAILABLE, error.code, error.message)
            })?;
        let state = states
            .into_iter()
            .find(|state| state.agent_id == agent.as_str())
            .ok_or_else(|| {
                ApiError::structured(
                    StatusCode::SERVICE_UNAVAILABLE,
                    crate::agent_model_control::AGENT_MODEL_CONTROL_FAILURE,
                    "Agent model control returned no state for the resolved Agent",
                )
            })?;
        let identity = state.effective.model;
        let model = if identity.provider_id == crate::support::model_ref::BARE_PROVIDER
            && identity.model_id != "offline"
        {
            identity.model_id
        } else {
            format!("{}/{}", identity.provider_id, identity.model_id)
        };
        return Ok(ModelRef::new(model));
    };

    Ok(explicit)
}

/// Bind once for `workdir` (or, with none, the project-less global view) and
/// list catalog agents from that TurnBinding.
///
/// Bind failures surface as typed `ApiError` (via `CoreError`) rather than an
/// empty list fallback — there is no second authority when binding fails.
pub(crate) async fn list(
    st: &ServerState,
    workdir: Option<&Path>,
) -> Result<Vec<BoundAgentRow>, ApiError> {
    let binding = match workdir {
        Some(workdir) => st.engine.bind_root_runtime(workdir).await?,
        None => st.engine.bind_global_runtime().await?,
    };
    let catalog = binding.agent_catalog();

    // Ordinary reachability: every non-reserved agent is reachable, because
    // built-ins spawn the whole ordinary set. Reserved system agents are not.
    let mut rows: Vec<BoundAgentRow> = catalog
        .all()
        .into_iter()
        .map(|agent| {
            let name = agent.stable_id;
            // Role is the sole selector rule (main → primary).
            let mode = agent.selector_mode().to_string();
            // Wire `hidden` preserves autocomplete exclusion for unreachable
            // subagents; it never affects the TUI selector.
            let hidden = mode == "subagent" && catalog.is_reserved(name);
            BoundAgentRow {
                name: name.to_string(),
                description: agent.description.map(str::to_string),
                mode,
                hidden,
                color: agent.color.map(str::to_string),
                prompt: agent.prompt.map(str::to_string),
                model: agent.model_policy.model.clone(),
            }
        })
        .collect();

    // default_agent fallback only — never merge agent definitions.
    let configured = st.default_agent.clone();
    sort_rows(&mut rows, configured.as_deref());
    Ok(rows)
}

/// Promote a configured default to the front only when it is a role-main row.
/// Invalid or subagent defaults keep pure name order (fail-closed).
fn sort_rows(agents: &mut [BoundAgentRow], configured_default: Option<&str>) {
    agents.sort_by(|left, right| {
        let left_default = is_promoted_default(left, configured_default);
        let right_default = is_promoted_default(right, configured_default);
        right_default
            .cmp(&left_default)
            .then_with(|| left.name.cmp(&right.name))
    });
}

fn is_promoted_default(agent: &BoundAgentRow, configured_default: Option<&str>) -> bool {
    match configured_default {
        Some(name) => agent.name == name && agent.mode == "primary",
        None => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
}
