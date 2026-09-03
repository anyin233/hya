use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hya_proto::SessionId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify};

use crate::agent_model_control::{
    AGENT_MODEL_CONFIGURED, AGENT_MODEL_CONTROL_FAILURE, AGENT_MODEL_CONTROL_UNAVAILABLE,
    AGENT_MODEL_INVALID_REQUEST, AGENT_MODEL_UNAVAILABLE, AGENT_MODEL_UNKNOWN_AGENT,
    AgentModelControlError, AgentModelIdentity, AgentModelState,
};
use crate::{ApiError, ServerState, parse_session};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/tui/bootstrap", get(bootstrap))
        .route("/tui/agent-models", get(agent_models))
        .route("/tui/agent-models/:agent_id", put(set_agent_model))
        .route("/tui/append-prompt", post(append_prompt))
        .route("/tui/open-help", post(open_help))
        .route("/tui/open-sessions", post(open_sessions))
        .route("/tui/open-themes", post(open_themes))
        .route("/tui/open-models", post(open_models))
        .route("/tui/submit-prompt", post(submit_prompt))
        .route("/tui/clear-prompt", post(clear_prompt))
        .route("/tui/execute-command", post(execute_command))
        .route("/tui/show-toast", post(show_toast))
        .route("/tui/publish", post(publish))
        .route("/tui/select-session", post(select_session))
        .route("/tui/control/next", get(control_next))
        .route("/tui/control/response", post(control_response))
}

/// Single-RTT payload for TUI startup sync (blocking + complete waves).
///
/// Command entries intentionally omit full prompt templates — expansion remains
/// server-side via `session.command` — so this stays small even with many skills.
async fn bootstrap(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let location = super::location::LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);

    let agent_models = if st.agent_model_control.available() {
        load_agent_models(&st, workdir.clone()).await?
    } else {
        Vec::new()
    };
    let (providers, provider_list) = super::catalog::bootstrap_provider_payload(&st);

    let home = home_dir();
    let path = json!({
        "home": home.to_string_lossy(),
        "state": env_path("XDG_STATE_HOME", &home, ".local/state/hya"),
        "config": env_path("XDG_CONFIG_HOME", &home, ".config/hya"),
        "worktree": workdir.to_string_lossy(),
        "directory": workdir.to_string_lossy(),
    });

    let project = json!({
        "id": workdir.to_string_lossy(),
        "worktree": workdir.to_string_lossy(),
    });

    let build_permissions = super::agent_permission::from_engine(&st.engine);
    // Bound catalog only: native=true, empty per-agent legacy overrides.
    let agents: Vec<Value> = super::bound_agent_metadata::list(&st, &workdir)
        .await?
        .into_iter()
        .map(|agent| {
            let model = agent.model.as_deref().unwrap_or(st.agent.model.as_str());
            let (provider_id, model_id) = match model.split_once('/') {
                Some((provider, model_id)) => (provider.to_string(), model_id.to_string()),
                None => ("hya".to_string(), model.to_string()),
            };
            json!({
                "name": agent.name,
                "description": agent.description,
                "mode": agent.mode,
                "native": true,
                "hidden": agent.hidden,
                "permission": super::instance::agent_permissions(
                    &agent.name,
                    &build_permissions,
                    Vec::new(),
                ),
                "model": { "modelID": model_id, "providerID": provider_id },
                "temperature": Value::Null,
                "topP": Value::Null,
                "color": agent.color,
                "steps": Value::Null,
                "prompt": if agent.name == "build" && agent.prompt.is_none() {
                    Some(st.agent.system_prompt.clone())
                } else {
                    agent.prompt
                },
                "options": json!({}),
            })
        })
        .collect();

    let commands: Vec<Value> = super::command_catalog::list(&workdir)
        .iter()
        .map(super::command_catalog::CommandInfo::bootstrap_summary)
        .collect();

    let sessions = match st.engine.store().list_sessions().await {
        Ok(list) => {
            let mut out = Vec::new();
            for session in list.into_iter().take(100) {
                if let Ok(snapshot) =
                    super::load_session(&st, session.session, Some(session.started_millis)).await
                {
                    if snapshot.info.empty_unnamed() {
                        continue;
                    }
                    if let Ok(value) = serde_json::to_value(&snapshot.info) {
                        out.push(value);
                    }
                }
            }
            out
        }
        Err(_) => Vec::new(),
    };
    let session_status = st.runs.statuses();
    let lsp = st.engine.lsp().status(&workdir).await.unwrap_or_default();
    let formatter = if st.formatter_status.is_empty() {
        st.engine
            .formatter()
            .status(&workdir)
            .await
            .unwrap_or_default()
    } else {
        st.formatter_status.clone()
    };
    let mcp = st.mcp_control.status().await;
    let mcp_resource = st.mcp_control.resources().await;
    let vcs = json!({
        "branch": super::instance::vcs::git::branch(&workdir),
        "default_branch": super::instance::vcs::git::default_branch(&workdir),
    });

    Ok(Json(json!({
        "config": st.global.config().await,
        "providers": providers,
        "provider_list": provider_list,
        "capabilities": {
            "backgroundSubagents": false,
            "agentModelPreferences": st.agent_model_control.available(),
        },
        "agentModels": agent_models,
        "agents": agents,
        "sessions": sessions,
        "commands": commands,
        "lsp": lsp,
        "mcp": mcp,
        "mcp_resource": mcp_resource,
        "formatter": formatter,
        "session_status": session_status,
        "vcs": vcs,
        "path": path,
        "project": project,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetAgentModelPayload {
    preference: Value,
}

async fn agent_models(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<Vec<AgentModelState>>, ApiError> {
    if !st.agent_model_control.available() {
        return Err(ApiError::structured(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            AGENT_MODEL_CONTROL_UNAVAILABLE,
            "Agent model control is unavailable",
        ));
    }
    let location = super::location::LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);
    Ok(Json(load_agent_models(&st, workdir).await?))
}

async fn set_agent_model(
    State(st): State<ServerState>,
    Path(agent_id): Path<String>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    payload: Result<Json<SetAgentModelPayload>, JsonRejection>,
) -> Result<Json<AgentModelState>, ApiError> {
    if !st.agent_model_control.available() {
        return Err(ApiError::structured(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            AGENT_MODEL_CONTROL_UNAVAILABLE,
            "Agent model control is unavailable",
        ));
    }
    let Json(payload) = payload.map_err(|error| {
        invalid_agent_model_request(format!("invalid Agent model preference body: {error}"))
    })?;
    let preference = if payload.preference.is_null() {
        None
    } else {
        Some(
            serde_json::from_value::<AgentModelIdentity>(payload.preference).map_err(|error| {
                invalid_agent_model_request(format!(
                    "invalid Agent model preference identity: {error}"
                ))
            })?,
        )
    };
    if let Some(identity) = preference.as_ref() {
        validate_identity(identity)?;
    }
    if agent_id.is_empty() {
        return Err(invalid_agent_model_request("Agent id must not be empty"));
    }
    let location = super::location::LocationRef::from_request(&query, &headers);
    let workdir = super::location::workdir_at(&st, &location);
    let binding = st.engine.bind_root_runtime(&workdir).await?;
    let base_model = st.agent.model.clone();
    st.agent_model_control
        .set(binding, agent_id, preference, base_model)
        .await
        .map(Json)
        .map_err(map_agent_model_error)
}

async fn load_agent_models(
    st: &ServerState,
    workdir: std::path::PathBuf,
) -> Result<Vec<AgentModelState>, ApiError> {
    let binding = st.engine.bind_root_runtime(&workdir).await?;
    st.agent_model_control
        .list(binding, st.agent.model.clone())
        .await
        .map_err(map_agent_model_error)
}

fn validate_identity(identity: &AgentModelIdentity) -> Result<(), ApiError> {
    if identity.provider_id.trim().is_empty() || identity.model_id.trim().is_empty() {
        return Err(invalid_agent_model_request(
            "Agent model providerID and modelID must not be empty",
        ));
    }
    if identity.provider_id.chars().count() > 1_024 {
        return Err(invalid_agent_model_request(
            "Agent model providerID is too long",
        ));
    }
    if identity.model_id.chars().count() > 4_096 {
        return Err(invalid_agent_model_request(
            "Agent model modelID is too long",
        ));
    }
    Ok(())
}

fn invalid_agent_model_request(message: impl Into<String>) -> ApiError {
    let error = AgentModelControlError::new(AGENT_MODEL_INVALID_REQUEST, message);
    ApiError::structured(
        axum::http::StatusCode::BAD_REQUEST,
        error.code,
        error.message,
    )
}

fn map_agent_model_error(error: AgentModelControlError) -> ApiError {
    let status = match error.code.as_str() {
        AGENT_MODEL_UNKNOWN_AGENT => axum::http::StatusCode::NOT_FOUND,
        AGENT_MODEL_CONFIGURED => axum::http::StatusCode::CONFLICT,
        AGENT_MODEL_UNAVAILABLE | AGENT_MODEL_INVALID_REQUEST => {
            axum::http::StatusCode::BAD_REQUEST
        }
        AGENT_MODEL_CONTROL_UNAVAILABLE | AGENT_MODEL_CONTROL_FAILURE => {
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        }
        _ => axum::http::StatusCode::SERVICE_UNAVAILABLE,
    };
    ApiError::structured(status, error.code, error.message)
}

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

fn env_path(key: &str, home: &std::path::Path, fallback: &str) -> String {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(|value| {
            std::path::PathBuf::from(value)
                .join("hya")
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| home.join(fallback).to_string_lossy().into_owned())
}

#[derive(Clone)]
pub(crate) struct TuiState {
    inner: Arc<TuiQueues>,
}

struct TuiQueues {
    requests: Mutex<VecDeque<TuiRequest>>,
    responses: Mutex<VecDeque<Value>>,
    request_ready: Notify,
}

#[derive(Clone, Debug, Serialize)]
struct TuiRequest {
    path: String,
    body: Value,
}

#[derive(Deserialize, Serialize)]
struct PublishPayload {
    #[serde(rename = "type")]
    kind: String,
    properties: Value,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AppendPromptPayload {
    text: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CommandPayload {
    command: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum ToastVariant {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ToastPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    message: String,
    variant: ToastVariant,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SelectSessionPayload {
    #[serde(rename = "sessionID")]
    session_id: String,
}

impl TuiState {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(TuiQueues {
                requests: Mutex::new(VecDeque::new()),
                responses: Mutex::new(VecDeque::new()),
                request_ready: Notify::new(),
            }),
        }
    }

    async fn push_request(&self, request: TuiRequest) {
        self.inner.requests.lock().await.push_back(request);
        self.inner.request_ready.notify_one();
    }

    async fn next_request(&self) -> TuiRequest {
        loop {
            if let Some(request) = self.inner.requests.lock().await.pop_front() {
                return request;
            }
            self.inner.request_ready.notified().await;
        }
    }

    async fn push_response(&self, response: Value) {
        self.inner.responses.lock().await.push_back(response);
    }
}

async fn append_prompt(
    State(st): State<ServerState>,
    Json(payload): Json<AppendPromptPayload>,
) -> Result<Json<bool>, ApiError> {
    queue_payload(&st, "/tui/append-prompt", payload).await
}

async fn open_help(State(st): State<ServerState>) -> Json<bool> {
    queue_empty(&st, "/tui/open-help").await
}

async fn open_sessions(State(st): State<ServerState>) -> Json<bool> {
    queue_empty(&st, "/tui/open-sessions").await
}

async fn open_themes(State(st): State<ServerState>) -> Json<bool> {
    queue_empty(&st, "/tui/open-themes").await
}

async fn open_models(State(st): State<ServerState>) -> Json<bool> {
    queue_empty(&st, "/tui/open-models").await
}

async fn submit_prompt(State(st): State<ServerState>) -> Json<bool> {
    queue_empty(&st, "/tui/submit-prompt").await
}

async fn clear_prompt(State(st): State<ServerState>) -> Json<bool> {
    queue_empty(&st, "/tui/clear-prompt").await
}

async fn execute_command(
    State(st): State<ServerState>,
    Json(payload): Json<CommandPayload>,
) -> Result<Json<bool>, ApiError> {
    queue_payload(&st, "/tui/execute-command", payload).await
}

async fn show_toast(
    State(st): State<ServerState>,
    Json(payload): Json<ToastPayload>,
) -> Result<Json<bool>, ApiError> {
    if payload.duration == Some(0) {
        return Err(ApiError::bad_request("duration must be positive"));
    }
    queue_payload(&st, "/tui/show-toast", payload).await
}

async fn publish(
    State(st): State<ServerState>,
    Json(payload): Json<PublishPayload>,
) -> Result<Json<bool>, ApiError> {
    validate_publish_type(&payload.kind)?;
    let body = serde_json::to_value(payload).map_err(|e| ApiError::internal(e.to_string()))?;
    queue_value(&st, "/tui/publish", body).await;
    Ok(Json(true))
}

async fn select_session(
    State(st): State<ServerState>,
    Json(payload): Json<SelectSessionPayload>,
) -> Result<Json<bool>, ApiError> {
    let session = parse_session(&payload.session_id)?;
    ensure_session(&st, session).await?;
    queue_payload(&st, "/tui/select-session", payload).await
}

async fn control_next(State(st): State<ServerState>) -> Json<TuiRequest> {
    Json(st.tui.next_request().await)
}

async fn control_response(State(st): State<ServerState>, Json(payload): Json<Value>) -> Json<bool> {
    st.tui.push_response(payload).await;
    Json(true)
}

fn validate_publish_type(kind: &str) -> Result<(), ApiError> {
    match kind {
        "tui.prompt.append" | "tui.command.execute" | "tui.toast.show" | "tui.session.select" => {
            Ok(())
        }
        _ => Err(ApiError::bad_request("unsupported TUI event type")),
    }
}

async fn queue_empty(st: &ServerState, path: &'static str) -> Json<bool> {
    queue_value(st, path, Value::Null).await;
    Json(true)
}

async fn queue_payload(
    st: &ServerState,
    path: &'static str,
    payload: impl Serialize,
) -> Result<Json<bool>, ApiError> {
    let body = serde_json::to_value(payload).map_err(|e| ApiError::internal(e.to_string()))?;
    queue_value(st, path, body).await;
    Ok(Json(true))
}

async fn queue_value(st: &ServerState, path: &'static str, body: Value) {
    st.tui
        .push_request(TuiRequest {
            path: path.to_string(),
            body,
        })
        .await;
}

async fn ensure_session(st: &ServerState, session: SessionId) -> Result<(), ApiError> {
    super::load_session(st, session, None).await.map(|_| ())
}
