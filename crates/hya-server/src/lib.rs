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
pub use state::AppState;
pub(crate) use state::ServerState;
pub use v1::V1Grpc;
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
/// Build the full HTTP app: the `/v1` contract routes + CORS.
///
/// The gRPC binding (`V1Grpc`) dispatches through this same router, so the
/// two transports share one handler set.
pub fn router(state: AppState) -> Router {
    let state = ServerState::new(state);
    v1::router().with_state(state).layer(cors())
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
