//! `hya-server` — Axum HTTP and SSE surface over `hya-core`.
//!
//! Serves the consolidated `hya.v1` contract:
//!
//! - **HTTP/JSON + SSE + WebSocket** — the `/v1` routes generated from the
//!   `hya.v1` IDL (`crates/hya-api`), plus the gRPC binding (`V1Grpc`)
//!   dispatching through the same router.
//!
//! CORS mirrors the request origin and headers and allows any method. See
//! `docs/protocol/` for the contract, integration guide, and generated
//! reference/OpenAPI.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use tower_http::cors::{AllowHeaders, AllowOrigin, Any, CorsLayer};

mod agent_model_control;
mod mcp_control;
mod pending;
mod provider_control;
mod runs;
mod state;
mod support;
mod v1;
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
pub use provider_control::{
    PROVIDER_CONTROL_FAILURE, PROVIDER_CONTROL_UNAVAILABLE, PROVIDER_INVALID_REQUEST,
    PROVIDER_NOT_FOUND, ProviderChange, ProviderControl, ProviderControlError,
    ProviderControlFuture, ProviderDiscoveryReport, ProviderKeySource, ProviderModelOverride,
    ProviderSettings, ProviderUpsert, valid_provider_id,
};
pub use state::AppState;
pub(crate) use state::ServerState;
pub use v1::V1Grpc;

/// Largest `CreateTurn` HTTP JSON body the server reads: room for the 20 MiB
/// prompt-attachment budget after base64 (4/3) plus the rest of the request.
/// A larger body fails in the transport (HTTP 413) before any validation.
pub const MAX_TURN_REQUEST_BYTES: usize = 32 * 1024 * 1024;

/// Largest `CreateTurn` gRPC message (`TurnServer::max_decoding_message_size`):
/// binary bytes that still fit [`MAX_TURN_REQUEST_BYTES`] once the binding
/// re-encodes them as protojson (base64) for the shared router. A larger
/// message fails with gRPC `out_of_range` before any validation.
pub const MAX_TURN_GRPC_MESSAGE_BYTES: usize = MAX_TURN_REQUEST_BYTES / 4 * 3;
pub use workflow_control::{
    WorkflowControl, WorkflowControlError, WorkflowControlFuture, WorkflowDecorationFuture,
};

/// Build the full HTTP app: the `/v1` contract routes + CORS.
///
/// The gRPC binding (`V1Grpc`) dispatches through this same router, so the
/// two transports share one handler set.
pub fn router(state: AppState) -> Router {
    let state = ServerState::new(state);
    spawn_background_reclaim_driver(state.clone());
    spawn_project_busy_watcher(state.clone());
    v1::router().with_state(state).layer(cors())
}

/// Drive the reclaim turn for backgrounded MCP calls.
///
/// The engine's background watcher admits the steered reclaim prompt first and
/// then publishes the completion marker (`ToolResult` metadata
/// `background_result`, or `ToolError` value `background_failed`). When the
/// session is idle at that moment, this driver starts one turn so the agent
/// reclaims the result immediately; a busy session is left alone — its turn
/// already sees the prompt on the next round.
fn spawn_background_reclaim_driver(state: ServerState) {
    let mut rx = state.engine.bus().subscribe();
    tokio::spawn(async move {
        loop {
            let session = match rx.recv().await {
                Ok(envelope) => background_completion_session(&envelope.event),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            };
            let Some(session) = session else { continue };
            if state.is_busy(session) {
                continue;
            }
            let Some(run) = state.start_run(session) else {
                continue;
            };
            let turn =
                crate::support::reference::session_agent_with_guidance(&state, session).await;
            let external_dirs =
                crate::support::reference::external_directories_at(&state, &turn.agent.workdir)
                    .await;
            let engine = state.engine.clone();
            let agent = turn.agent.clone();
            let guidance = turn.guidance.clone();
            tokio::spawn(async move {
                let _ = engine
                    .run_turn_with_external_dirs_and_guidance(
                        session,
                        &agent,
                        run.token(),
                        &external_dirs,
                        guidance,
                        None,
                    )
                    .await;
                drop(run);
            });
        }
    });
}

/// Publish `projectsUpdated` whenever the set of busy Projects changes.
///
/// A session becomes busy only by starting a turn or a Workflow run, which
/// appends an event on the bus; its run registry entry and turn lease are
/// released shortly after the closing event. So the watcher tracks every
/// session a turn-boundary event named, re-checks the tracked ones on each
/// such event and every 100 ms while any is tracked, and drops a session
/// once it is idle. A tracked session counts toward its Project only while
/// it is not archived (`UpdateSession.archived` publishes directly).
fn spawn_project_busy_watcher(state: ServerState) {
    use std::collections::{BTreeSet, HashMap};

    let mut rx = state.engine.bus().subscribe();
    tokio::spawn(async move {
        let mut tracked: HashMap<hya_proto::SessionId, Option<hya_proto::ProjectId>> =
            HashMap::new();
        let mut busy_projects: BTreeSet<hya_proto::ProjectId> = BTreeSet::new();
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                received = rx.recv() => match received {
                    Ok(envelope) => {
                        let Some(session) = turn_boundary_session(&envelope.event) else {
                            continue;
                        };
                        if let std::collections::hash_map::Entry::Vacant(slot) =
                            tracked.entry(session)
                        {
                            slot.insert(
                                state
                                    .engine
                                    .read_projection_shared(session)
                                    .await
                                    .ok()
                                    .and_then(|projection| projection.session.project),
                            );
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                },
                _ = tick.tick(), if !tracked.is_empty() => {}
            }
            tracked.retain(|session, _| state.is_busy(*session));
            let mut now = BTreeSet::new();
            for (session, project) in &tracked {
                let Some(project) = project else { continue };
                if now.contains(project) {
                    continue;
                }
                let archived = state
                    .engine
                    .read_projection_shared(*session)
                    .await
                    .is_ok_and(|projection| projection.session.is_archived());
                if !archived {
                    now.insert(*project);
                }
            }
            if now != busy_projects {
                busy_projects = now;
                state.notify_projects_updated();
            }
        }
    });
}

/// The session whose busy state an event may flip: turn and Workflow run
/// boundaries.
fn turn_boundary_session(event: &hya_proto::Event) -> Option<hya_proto::SessionId> {
    match event {
        hya_proto::Event::MessageStarted { session, .. }
        | hya_proto::Event::MessageFinished { session, .. }
        | hya_proto::Event::WorkflowRunStarted { session, .. }
        | hya_proto::Event::WorkflowRunFinished { session, .. } => Some(*session),
        _ => None,
    }
}

/// The session of a backgrounded-MCP completion marker, if the event is one.
fn background_completion_session(event: &hya_proto::Event) -> Option<hya_proto::SessionId> {
    match event {
        hya_proto::Event::ToolResult {
            session, output, ..
        } => output
            .get("metadata")
            .and_then(|metadata| metadata.get("background_result"))
            .map(|_| *session),
        hya_proto::Event::ToolError { session, value, .. } => value
            .as_ref()
            .and_then(|value| value.get("background_failed"))
            .map(|_| *session),
        _ => None,
    }
}

fn cors() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::mirror_request())
        .allow_headers(AllowHeaders::mirror_request())
        .allow_methods(Any)
}

/// HTTP error returned by `/v1` handlers as `(status, message)`.
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

    pub(crate) fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    pub(crate) fn text(&self) -> &str {
        &self.message
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::with_status(StatusCode::BAD_REQUEST, message)
    }

    #[allow(dead_code)]
    fn not_found(message: impl Into<String>) -> Self {
        Self::with_status(StatusCode::NOT_FOUND, message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::with_status(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    #[allow(dead_code)]
    fn conflict(message: impl Into<String>) -> Self {
        Self::with_status(StatusCode::CONFLICT, message)
    }

    #[allow(dead_code)]
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
