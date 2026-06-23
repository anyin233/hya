use std::collections::VecDeque;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, Notify};
use yaca_proto::SessionId;

use crate::{ApiError, ServerState, parse_session};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
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
    if !payload.session_id.starts_with("ses") {
        return Err(ApiError::bad_request("invalid session id"));
    }
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
