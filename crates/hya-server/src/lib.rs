//! `hya-server` — Axum HTTP and SSE surface over `hya-core`.
//!
//! Serves:
//!
//! - **Native routes** — `POST /sessions`, `POST /sessions/:id/{prompt,command,shell}`,
//!   `GET /sessions/:id/events`, `GET /sessions/:id/stream`
//! - **Compat route groups** (via `compat::router`) — sessions, events, files/search,
//!   catalogs/metadata, provider/auth, permissions/questions, MCP, PTY, VCS/project,
//!   worktree, TUI/global/sync/experimental surfaces
//!
//! CORS mirrors the request origin and headers and allows any method. See
//! `docs/architecture/server-client.md` for request bodies and status codes.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use futures::StreamExt;
use hya_core::CreateSession;
use hya_proto::api::{
    CommandRequest, CreateSessionRequest, CreateSessionResponse, EventsQuery, PromptRequest,
    PromptResponse, ShellRequest,
};
use hya_proto::{Envelope, ModelRef, SessionId};
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::{AllowHeaders, AllowOrigin, Any, CorsLayer};

mod agent_model_control;
mod compat;
mod mcp_control;
mod pending;
mod runs;
mod state;
mod workflow;
mod workflow_control;

pub use agent_model_control::{
    AGENT_MODEL_CONFIGURED, AGENT_MODEL_CONTROL_FAILURE, AGENT_MODEL_CONTROL_UNAVAILABLE,
    AGENT_MODEL_INVALID_REQUEST, AGENT_MODEL_UNAVAILABLE, AGENT_MODEL_UNKNOWN_AGENT,
    AgentModelControl, AgentModelControlError, AgentModelControlFuture, AgentModelEffective,
    AgentModelIdentity, AgentModelSource, AgentModelState,
};
pub use hya_proto::WorkspaceAdapterInfo;
pub use hya_tool::FormatterStatus;
pub use mcp_control::McpControl;
pub use state::AppState;
pub(crate) use state::ServerState;
pub use workflow_control::{
    WorkflowControl, WorkflowControlError, WorkflowControlFuture, WorkflowDecorationFuture,
};

/// Build the full HTTP app: Compat routes + native session routes + CORS.
///
/// Native paths:
/// - `POST /sessions` — create session
/// - `POST /sessions/:id/prompt` — admit user prompt and run one turn
/// - `POST /sessions/:id/command` — admit command prompt and run one turn
/// - `POST /sessions/:id/shell` — run shell tool turn
/// - `GET /sessions/:id/workflow` — return projected Workflow state
/// - `POST /sessions/:id/workflow` — execute a typed Workflow command
/// - `GET /sessions/:id/events` — replay envelopes (`?since_seq=`)
/// - `GET /sessions/:id/stream` — SSE of live envelopes (emits `resync` on lag)
///
/// Merges `compat::router()` for Compat-compatible surfaces. CORS:
/// `AllowOrigin::mirror_request()`, `AllowHeaders::mirror_request()`, methods `Any`.
pub fn router(state: AppState) -> Router {
    let state = ServerState::new(state);
    Router::new()
        .merge(compat::router())
        .merge(workflow::native_router())
        .merge(workflow::compat_router())
        .route("/sessions", post(create_session))
        .route("/sessions/:id/prompt", post(prompt))
        .route("/sessions/:id/command", post(command))
        .route("/sessions/:id/shell", post(shell))
        .route("/sessions/:id/events", get(events))
        .route("/sessions/:id/stream", get(stream))
        .with_state(state)
        .layer(cors())
}

fn cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_headers(AllowHeaders::mirror_request())
        .allow_methods(Any)
}

/// HTTP error returned by native and many Compat handlers as `(status, message)`.
///
/// Constructed via private helpers (`bad_request`, `not_found`, `conflict`,
/// `service_unavailable`, `internal`). `CoreError` / `StoreError` map to 500.
pub struct ApiError {
    status: StatusCode,
    message: String,
    code: Option<String>,
}

impl ApiError {
    fn with_status(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            code: None,
        }
    }

    pub(crate) fn structured(
        status: StatusCode,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            message: message.into(),
            code: Some(code.into()),
        }
    }

    pub(crate) fn workflow(error: crate::WorkflowControlError) -> Self {
        let status = crate::workflow::error_status(&error);
        Self::structured(status, error.code, error.message)
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::with_status(StatusCode::BAD_REQUEST, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::with_status(StatusCode::NOT_FOUND, message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::with_status(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::with_status(StatusCode::CONFLICT, message)
    }

    fn service_unavailable(message: impl Into<String>) -> Self {
        Self::with_status(StatusCode::SERVICE_UNAVAILABLE, message)
    }
}

impl From<hya_core::CoreError> for ApiError {
    fn from(e: hya_core::CoreError) -> Self {
        Self::internal(e.to_string())
    }
}

impl From<hya_store::StoreError> for ApiError {
    fn from(e: hya_store::StoreError) -> Self {
        Self::internal(e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Some(code) = self.code {
            return (
                self.status,
                Json(serde_json::json!({
                    "error": { "code": code, "message": self.message }
                })),
            )
                .into_response();
        }
        (self.status, self.message).into_response()
    }
}

fn parse_session(id: &str) -> Result<SessionId, ApiError> {
    id.parse::<SessionId>()
        .map_err(|_| ApiError::bad_request("invalid session id"))
}

/// Reject native Session routes after deletion or for unknown identifiers.
async fn ensure_session_exists(st: &ServerState, session: SessionId) -> Result<(), ApiError> {
    if st.engine.session_exists(session).await? {
        Ok(())
    } else {
        Err(ApiError::not_found(format!("session not found: {session}")))
    }
}

async fn create_session(
    State(st): State<ServerState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, ApiError> {
    let agent = compat::bound_agent_metadata::resolve_session_agent(
        &st,
        std::path::Path::new(&req.workdir),
        Some(req.agent.as_str()),
    )
    .await?;
    let session = st
        .engine
        .create(CreateSession {
            parent: req.parent,
            agent,
            model: ModelRef::new(req.model),
            workdir: req.workdir,
        })
        .await?;
    Ok(Json(CreateSessionResponse { session }))
}

async fn prompt(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<PromptRequest>,
) -> Result<Json<PromptResponse>, ApiError> {
    let session = parse_session(&id)?;
    ensure_session_exists(&st, session).await?;
    let run = st
        .start_run(session)
        .ok_or_else(|| ApiError::conflict("session busy"))?;
    let message = st.engine.admit_user_prompt(session, req.text).await?;
    let finish = st.engine.run_turn(session, &st.agent, run.token()).await?;
    Ok(Json(PromptResponse { message, finish }))
}

async fn command(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<CommandRequest>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    ensure_session_exists(&st, session).await?;
    if let Some(result) = workflow::intercept_slash(&st, session, &req).await? {
        return Ok(Json(result).into_response());
    }
    let run = st
        .start_run(session)
        .ok_or_else(|| ApiError::conflict("session busy"))?;
    if let Some(model) = req.model_ref() {
        st.engine.switch_model(session, model).await?;
    }
    let CommandRequest {
        command,
        arguments,
        text,
        ..
    } = req;
    let text = text.unwrap_or_else(|| command_prompt_text(&command, &arguments));
    let message = st
        .engine
        .admit_command_prompt(session, command, arguments, text)
        .await?;
    let finish = st.engine.run_turn(session, &st.agent, run.token()).await?;
    Ok(Json(PromptResponse { message, finish }).into_response())
}

async fn shell(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<ShellRequest>,
) -> Result<Json<PromptResponse>, ApiError> {
    let session = parse_session(&id)?;
    ensure_session_exists(&st, session).await?;
    let run = st
        .start_run(session)
        .ok_or_else(|| ApiError::conflict("session busy"))?;
    let agent = compat::shell_agent(&st, session, &req).await?;
    let (message, finish) = st
        .engine
        .run_shell(session, &agent, req.command, run.token())
        .await?;
    Ok(Json(PromptResponse { message, finish }))
}

fn command_prompt_text(command: &str, arguments: &str) -> String {
    if arguments.trim().is_empty() {
        format!("/{command}")
    } else {
        format!("/{command} {arguments}")
    }
}

async fn events(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<Envelope>>, ApiError> {
    let session = parse_session(&id)?;
    let since = q.since_seq.unwrap_or(0);
    let envelopes = st.engine.replay(session).await?;
    if envelopes.is_empty() {
        return Err(ApiError::not_found(format!("session not found: {session}")));
    }
    let envelopes = envelopes.into_iter().filter(|e| e.seq.0 > since).collect();
    Ok(Json(envelopes))
}

async fn stream(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let session = parse_session(&id)?;
    ensure_session_exists(&st, session).await?;
    let rx = st.engine.bus().subscribe();
    let events = BroadcastStream::new(rx).filter_map(move |result| async move {
        match result {
            Ok(env) if env.event.session() == Some(session) => {
                Some(Ok(SseEvent::default().json_data(&env).unwrap_or_default()))
            }
            Ok(_) => None,
            Err(_lagged) => Some(Ok(SseEvent::default().event("resync"))),
        }
    });
    Ok(Sse::new(events))
}
