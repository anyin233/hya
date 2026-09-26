//! `/v1` agent-models domain: durable per-agent base-model preferences over
//! the app-owned control handle.

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, put};

use super::Json;
use hya_api::v1 as pb;
use hya_proto::SessionId;

use crate::ServerState;
use crate::agent_model_control::{AgentModelControlError, AgentModelIdentity};

use super::{V1Error, catalog_scope};

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/agent-models", get(list_agent_models))
        .route("/v1/agent-models/:agent_id", put(set_agent_model))
}

fn selection(identity: &AgentModelIdentity) -> pb::AgentModelSelection {
    pb::AgentModelSelection {
        provider_id: identity.provider_id.clone(),
        model_id: identity.model_id.clone(),
    }
}

fn source(source: crate::agent_model_control::AgentModelSource) -> i32 {
    use crate::agent_model_control::AgentModelSource as S;
    match source {
        S::Session => pb::AgentModelSource::Session as i32,
        S::Configured => pb::AgentModelSource::Configured as i32,
        S::Remembered => pb::AgentModelSource::Remembered as i32,
        S::Default => pb::AgentModelSource::Default as i32,
    }
}

fn state_row(row: &crate::agent_model_control::AgentModelState) -> pb::AgentModelState {
    pb::AgentModelState {
        agent_id: row.agent_id.clone(),
        description: row.description.clone().unwrap_or_default(),
        mode: row.mode.clone(),
        hidden: row.hidden,
        configured: row.configured,
        settable: row.settable,
        preference: row.preference.as_ref().map(selection),
        preference_available: row.preference_available,
        effective: Some(selection(&row.effective.model)),
        source: source(row.effective.source),
        configuration: row.configuration.as_ref().map(selection),
        session_override: row.session_override.as_ref().map(selection),
    }
}

fn map_control_error(error: AgentModelControlError) -> V1Error {
    use crate::agent_model_control as codes;
    match error.code.as_str() {
        codes::AGENT_MODEL_UNKNOWN_AGENT => {
            V1Error::new(hya_api::error::Code::NotFound, error.message)
        }
        codes::AGENT_MODEL_CONFIGURED => {
            V1Error::new(hya_api::error::Code::Conflict, error.message)
        }
        codes::AGENT_MODEL_CONTROL_UNAVAILABLE | codes::AGENT_MODEL_UNAVAILABLE => {
            V1Error::unavailable(error.message)
        }
        codes::AGENT_MODEL_INVALID_REQUEST => V1Error::invalid_argument(error.message),
        _ => V1Error::internal(error.message),
    }
}

fn ensure_available(st: &ServerState) -> Result<(), V1Error> {
    if st.agent_model_control.available() {
        Ok(())
    } else {
        Err(V1Error::unavailable("agent model control is unavailable"))
    }
}

async fn parse_scope_session(session: &str) -> Result<Option<SessionId>, V1Error> {
    if session.trim().is_empty() {
        return Ok(None);
    }
    session
        .parse::<SessionId>()
        .map(Some)
        .map_err(|_| V1Error::invalid_argument(format!("invalid session id: {session}")))
}

/// Bind against the session runtime (at the session's workdir) when a
/// session is supplied, otherwise the request's directory scope (its Project
/// when it lies inside one, [`catalog_scope`]), otherwise the global
/// (project-less) binding.
async fn model_binding(
    st: &ServerState,
    headers: &HeaderMap,
    directory: &str,
    session: Option<SessionId>,
) -> Result<hya_core::TurnBinding, V1Error> {
    match session {
        Some(session) => {
            if !st.engine.session_exists(session).await? {
                return Err(V1Error::session_not_found(&session.to_string()));
            }
            let workdir = crate::support::reference::session_workdir(st, session).await?;
            Ok(st.engine.bind_session_runtime(session, &workdir).await?)
        }
        None => Ok(catalog_scope(st, headers, directory)
            .await?
            .bind(st)
            .await?),
    }
}

async fn list_agent_models(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ListAgentModelsResponse>, V1Error> {
    ensure_available(&st)?;
    let request: pb::ListAgentModelsRequest = super::query_request(&[], &query)?;
    let session = parse_scope_session(&request.session).await?;
    let binding = model_binding(&st, &headers, &request.directory, session).await?;
    let rows = st
        .agent_model_control
        .list(binding, st.agent.model.clone())
        .await
        .map_err(map_control_error)?;
    Ok(Json(pb::ListAgentModelsResponse {
        agents: rows.iter().map(state_row).collect(),
    }))
}

fn validate_identity(selection: &pb::AgentModelSelection) -> Result<(), V1Error> {
    if selection.provider_id.trim().is_empty() || selection.model_id.trim().is_empty() {
        return Err(V1Error::invalid_argument(
            "agent model providerId and modelId must not be empty",
        ));
    }
    if selection.provider_id.chars().count() > 1_024 {
        return Err(V1Error::invalid_argument(
            "agent model providerId is too long",
        ));
    }
    if selection.model_id.chars().count() > 4_096 {
        return Err(V1Error::invalid_argument("agent model modelId is too long"));
    }
    Ok(())
}

async fn set_agent_model(
    State(st): State<ServerState>,
    AxumPath(agent_id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<pb::AgentModelState>, V1Error> {
    ensure_available(&st)?;
    let scope: pb::ListAgentModelsRequest = super::query_request(&[], &query)?;
    let preference = match body.as_ref().and_then(|Json(value)| value.as_object()) {
        Some(map) => match map.get("preference") {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => {
                let selection: pb::AgentModelSelection = serde_json::from_value(value.clone())
                    .map_err(|error| {
                        V1Error::invalid_argument(format!(
                            "invalid agent model preference: {error}"
                        ))
                    })?;
                validate_identity(&selection)?;
                Some(AgentModelIdentity::new(
                    selection.provider_id,
                    selection.model_id,
                ))
            }
        },
        _ => None,
    };
    let session = parse_scope_session(&scope.session).await?;
    let binding = model_binding(&st, &headers, &scope.directory, session).await?;
    let row = st
        .agent_model_control
        .set(binding, agent_id, preference, st.agent.model.clone())
        .await
        .map_err(map_control_error)?;
    Ok(Json(state_row(&row)))
}
