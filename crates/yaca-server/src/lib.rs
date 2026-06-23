//! `yaca-server` — axum HTTP + SSE over `yaca-core` (design.md §11).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use futures::StreamExt;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::{AllowHeaders, AllowOrigin, Any, CorsLayer};
use yaca_core::CreateSession;
use yaca_proto::api::{
    CommandRequest, CreateSessionRequest, CreateSessionResponse, EventsQuery, PromptRequest,
    PromptResponse, ShellRequest,
};
use yaca_proto::{AgentName, Envelope, ModelRef, SessionId};

mod opencode;
mod pending;
mod runs;
mod state;

pub use state::AppState;
pub(crate) use state::ServerState;
pub use yaca_proto::WorkspaceAdapterInfo;
pub use yaca_tool::FormatterStatus;

pub fn router(state: AppState) -> Router {
    let state = ServerState::new(state);
    Router::new()
        .merge(opencode::router())
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

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn with_status(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
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

impl From<yaca_core::CoreError> for ApiError {
    fn from(e: yaca_core::CoreError) -> Self {
        Self::internal(e.to_string())
    }
}

impl From<yaca_store::StoreError> for ApiError {
    fn from(e: yaca_store::StoreError) -> Self {
        Self::internal(e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}

fn parse_session(id: &str) -> Result<SessionId, ApiError> {
    id.parse::<SessionId>()
        .map_err(|_| ApiError::bad_request("invalid session id"))
}

async fn create_session(
    State(st): State<ServerState>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<CreateSessionResponse>, ApiError> {
    let session = st
        .engine
        .create(CreateSession {
            parent: req.parent,
            agent: AgentName::new(req.agent),
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
    let run = st
        .runs
        .start(session)
        .ok_or_else(|| ApiError::conflict("session busy"))?;
    let message = st.engine.admit_user_prompt(session, req.text).await?;
    let finish = st.engine.run_turn(session, &st.agent, run.token()).await?;
    Ok(Json(PromptResponse { message, finish }))
}

async fn command(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<CommandRequest>,
) -> Result<Json<PromptResponse>, ApiError> {
    let session = parse_session(&id)?;
    let run = st
        .runs
        .start(session)
        .ok_or_else(|| ApiError::conflict("session busy"))?;
    let text = req
        .text
        .unwrap_or_else(|| command_prompt_text(&req.command, &req.arguments));
    let message = st
        .engine
        .admit_command_prompt(session, req.command, req.arguments, text)
        .await?;
    let finish = st.engine.run_turn(session, &st.agent, run.token()).await?;
    Ok(Json(PromptResponse { message, finish }))
}

async fn shell(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<ShellRequest>,
) -> Result<Json<PromptResponse>, ApiError> {
    let session = parse_session(&id)?;
    let run = st
        .runs
        .start(session)
        .ok_or_else(|| ApiError::conflict("session busy"))?;
    let (message, finish) = st
        .engine
        .run_shell(session, &st.agent, req.command, run.token())
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
    let envelopes = st
        .engine
        .replay(session)
        .await?
        .into_iter()
        .filter(|e| e.seq.0 > since)
        .collect();
    Ok(Json(envelopes))
}

async fn stream(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let session = parse_session(&id)?;
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
