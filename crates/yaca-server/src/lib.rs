//! `yaca-server` — axum HTTP + SSE over `yaca-core` (design.md §11).

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::Stream;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::{AllowHeaders, AllowOrigin, Any, CorsLayer};
use yaca_core::{AgentSpec, CreateSession, SessionEngine};
use yaca_mcp::McpManager;
use yaca_proto::api::{
    CommandRequest, CreateSessionRequest, CreateSessionResponse, EventsQuery, PromptRequest,
    PromptResponse, ShellRequest,
};
use yaca_proto::{AgentName, Envelope, ModelRef, SessionId};
use yaca_tool::{AskRequest, QuestionRequest};

mod opencode;
mod pending;
mod runs;

#[derive(Clone)]
pub struct AppState {
    pub engine: Arc<SessionEngine>,
    pub agent: Arc<AgentSpec>,
    permission_requests: pending::PermissionRequests,
    question_requests: pending::QuestionRequests,
    mcp_manager: Arc<McpManager>,
}

impl AppState {
    #[must_use]
    pub fn new(engine: Arc<SessionEngine>, agent: Arc<AgentSpec>) -> Self {
        Self {
            engine,
            agent,
            permission_requests: Default::default(),
            question_requests: Default::default(),
            mcp_manager: Default::default(),
        }
    }

    #[must_use]
    pub fn with_permission_requests(mut self, rx: mpsc::UnboundedReceiver<AskRequest>) -> Self {
        self.permission_requests = pending::PermissionRequests::spawn(rx);
        self
    }

    #[must_use]
    pub fn with_question_requests(mut self, rx: mpsc::UnboundedReceiver<QuestionRequest>) -> Self {
        self.question_requests = pending::QuestionRequests::spawn(rx);
        self
    }

    #[must_use]
    pub fn with_mcp_manager(mut self, manager: McpManager) -> Self {
        self.mcp_manager = Arc::new(manager);
        self
    }
}

#[derive(Clone)]
struct ServerState {
    engine: Arc<SessionEngine>,
    agent: Arc<AgentSpec>,
    runs: runs::RunRegistry,
    permission_requests: pending::PermissionRequests,
    question_requests: pending::QuestionRequests,
    global: opencode::GlobalState,
    mcp_manager: Arc<McpManager>,
    mcp_http: opencode::McpHttpState,
    project: opencode::ProjectState,
    pty: opencode::PtyState,
    tui: opencode::TuiState,
}

impl ServerState {
    fn new(app: AppState) -> Self {
        Self {
            engine: app.engine,
            agent: app.agent,
            runs: runs::RunRegistry::default(),
            permission_requests: app.permission_requests,
            question_requests: app.question_requests,
            global: opencode::GlobalState::new(),
            mcp_manager: app.mcp_manager,
            mcp_http: opencode::McpHttpState::new(),
            project: opencode::ProjectState::new(),
            pty: opencode::PtyState::new(),
            tui: opencode::TuiState::new(),
        }
    }
}

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
