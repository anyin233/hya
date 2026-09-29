//! `/v1` agent-models domain: durable per-agent base-model preferences over
//! the app-owned control handle.

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
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
        .route(
            "/v1/agent-models/:agent_id/configuration",
            put(save_agent_model_configuration),
        )
        .route(
            "/v1/model-effort-preferences",
            get(list_model_effort_preferences),
        )
        .route(
            "/v1/model-effort-preferences/:provider_id/:model_id",
            put(set_model_effort_preference),
        )
        .route("/v1/agent-efforts/:agent_id", put(set_agent_effort))
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
        effort: String::new(),
        effort_source: pb::AgentEffortSource::None as i32,
        configuration_path: row.configuration_path.clone().unwrap_or_default(),
    }
}

/// Parse a `SetAgentModel`/`SaveAgentModelConfiguration` PUT body and bind it.
///
/// `directory` and `session` are body fields of these PUTs (protojson
/// mapping); the query string is not consulted. `field` names the optional
/// `AgentModelSelection` (absent or `null` clears), and `label` words its
/// decode error.
async fn agent_model_body(
    st: &ServerState,
    body: Option<&serde_json::Value>,
    field: &str,
    label: &str,
) -> Result<(hya_core::TurnBinding, Option<AgentModelIdentity>), V1Error> {
    let body_text = |name: &str| -> Result<String, V1Error> {
        match body.and_then(|value| value.get(name)) {
            None | Some(serde_json::Value::Null) => Ok(String::new()),
            Some(serde_json::Value::String(text)) => Ok(text.clone()),
            Some(_) => Err(V1Error::invalid_argument(format!(
                "invalid request body: `{name}` must be a string"
            ))),
        }
    };
    let directory = body_text("directory")?;
    let session = body_text("session")?;
    let selection = match body.and_then(|value| value.get(field)) {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => {
            let selection: pb::AgentModelSelection = serde_json::from_value(value.clone())
                .map_err(|error| V1Error::invalid_argument(format!("invalid {label}: {error}")))?;
            validate_identity(&selection)?;
            Some(AgentModelIdentity::new(
                selection.provider_id,
                selection.model_id,
            ))
        }
    };
    let session = parse_scope_session(&session).await?;
    let binding = model_binding(st, &directory, session).await?;
    Ok((binding, selection))
}

/// Write an agent's model into its owning configuration file (live), keeping
/// any distinct session override.
async fn save_agent_model_configuration(
    State(st): State<ServerState>,
    AxumPath(agent_id): AxumPath<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<pb::AgentModelState>, V1Error> {
    ensure_available(&st)?;
    let body = body.map(|Json(value)| value);
    let (binding, model) = agent_model_body(&st, body.as_ref(), "model", "agent model").await?;
    let row = st
        .agent_model_control
        .save_configuration(binding.clone(), agent_id, model, st.agent.model.clone())
        .await
        .map_err(map_control_error)?;
    super::providers::notify_catalog_updated(&st);
    Ok(Json(
        with_agent_effort(&st, &binding, state_row(&row)).await?,
    ))
}

/// Fill the row's default effort with the same Agent layer the turn loop
/// uses: runtime choice > `agents.<id>.reasoning` > authored policy.
async fn with_agent_effort(
    st: &ServerState,
    binding: &hya_core::TurnBinding,
    mut row: pb::AgentModelState,
) -> Result<pb::AgentModelState, V1Error> {
    let authored = binding
        .agent_catalog()
        .resolve(&row.agent_id)
        .and_then(|definition| definition.model_policy.reasoning.clone())
        .and_then(|label| hya_provider::ReasoningEffort::parse(&label));
    if let Some((effort, source)) = st.engine.agent_effort(&row.agent_id, authored).await? {
        row.effort = effort.as_str().to_string();
        row.effort_source = match source {
            hya_core::AgentEffortSource::Preference => pb::AgentEffortSource::Preference,
            hya_core::AgentEffortSource::Configured => pb::AgentEffortSource::Configured,
            hya_core::AgentEffortSource::Authored => pb::AgentEffortSource::Authored,
        } as i32;
    }
    Ok(row)
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
        None => Ok(catalog_scope(st, directory).await?.bind(st).await?),
    }
}

async fn list_agent_models(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::ListAgentModelsResponse>, V1Error> {
    ensure_available(&st)?;
    let request: pb::ListAgentModelsRequest = super::query_request(&[], &query)?;
    let session = parse_scope_session(&request.session).await?;
    let binding = model_binding(&st, &request.directory, session).await?;
    let rows = st
        .agent_model_control
        .list(binding.clone(), st.agent.model.clone())
        .await
        .map_err(map_control_error)?;
    let mut agents = Vec::with_capacity(rows.len());
    for row in &rows {
        agents.push(with_agent_effort(&st, &binding, state_row(row)).await?);
    }
    Ok(Json(pb::ListAgentModelsResponse { agents }))
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
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<pb::AgentModelState>, V1Error> {
    ensure_available(&st)?;
    let body = body.map(|Json(value)| value);
    let (binding, preference) =
        agent_model_body(&st, body.as_ref(), "preference", "agent model preference").await?;
    let row = st
        .agent_model_control
        .set(
            binding.clone(),
            agent_id,
            preference,
            st.agent.model.clone(),
        )
        .await
        .map_err(map_control_error)?;
    Ok(Json(
        with_agent_effort(&st, &binding, state_row(&row)).await?,
    ))
}

async fn list_model_effort_preferences(
    State(st): State<ServerState>,
) -> Result<Json<pb::ListModelEffortPreferencesResponse>, V1Error> {
    ensure_available(&st)?;
    let preferences = st
        .agent_model_control
        .list_model_effort_preferences()
        .await
        .map_err(map_control_error)?;
    Ok(Json(pb::ListModelEffortPreferencesResponse {
        preferences: preferences
            .into_iter()
            .map(|row| pb::ModelEffortPreference {
                provider_id: row.provider_id,
                model_id: row.model_id,
                effort: row.effort,
                updated_at: row.updated_at,
            })
            .collect(),
    }))
}

async fn set_model_effort_preference(
    State(st): State<ServerState>,
    AxumPath((provider_id, model_id)): AxumPath<(String, String)>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<pb::ModelEffortPreference>, V1Error> {
    ensure_available(&st)?;
    let effort = body
        .as_ref()
        .and_then(|Json(value)| value.get("effort"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    st.agent_model_control
        .set_model_effort_preference(provider_id.clone(), model_id.clone(), effort.to_string())
        .await
        .map_err(map_control_error)?;
    // Every session on this model may now resolve another effort: clients
    // re-read their session rows on this live frame.
    super::providers::notify_catalog_updated(&st);
    let row = st
        .agent_model_control
        .list_model_effort_preferences()
        .await
        .map_err(map_control_error)?
        .into_iter()
        .find(|row| row.provider_id == provider_id && row.model_id == model_id);
    Ok(Json(row.map_or(
        pb::ModelEffortPreference {
            provider_id,
            model_id,
            effort: String::new(),
            updated_at: 0,
        },
        |row| pb::ModelEffortPreference {
            provider_id: row.provider_id,
            model_id: row.model_id,
            effort: row.effort,
            updated_at: row.updated_at,
        },
    )))
}

async fn set_agent_effort(
    State(st): State<ServerState>,
    AxumPath(agent_id): AxumPath<String>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<pb::AgentEffort>, V1Error> {
    ensure_available(&st)?;
    let effort = match body.as_ref().and_then(|Json(value)| value.get("effort")) {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(text)) => text.trim().to_string(),
        Some(_) => {
            return Err(V1Error::invalid_argument(
                "invalid request body: `effort` must be a string",
            ));
        }
    };
    if !effort.is_empty() && hya_provider::ReasoningEffort::parse(&effort).is_none() {
        return Err(V1Error::invalid_argument(format!(
            "unknown thinking effort `{effort}`"
        )));
    }
    let directory = body
        .as_ref()
        .and_then(|Json(value)| value.get("directory"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let binding = catalog_scope(&st, directory).await?.bind(&st).await?;
    if binding.agent_catalog().resolve(&agent_id).is_none() {
        return Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("unknown Agent `{agent_id}`"),
        ));
    }
    st.agent_model_control
        .set_agent_effort(
            agent_id.clone(),
            (!effort.is_empty()).then(|| effort.clone()),
        )
        .await
        .map_err(map_control_error)?;
    super::providers::notify_catalog_updated(&st);
    Ok(Json(pb::AgentEffort { agent_id, effort }))
}
