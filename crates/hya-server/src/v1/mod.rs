//! `hya-server` v1 HTTP binding: the `/v1` routes generated from the
//! `hya.v1` contract (`crates/hya-api`).
//!
//! Every handler speaks the generated protojson types directly, renders
//! failures through the stable error model (`hya_api::error`), and shares
//! the same engine/app state as the legacy surface. The gRPC binding
//! (phase P3) wraps the same handler logic.

mod agent_models;
mod auth;
mod bundle_api;
mod catalog;
mod convert;
mod events;
mod fs;
mod grpc;
mod interaction;
mod logs;
mod mcp;
mod message;
mod process;
mod project;
mod pty;
mod session;
mod turn;
mod workflow;
mod worktree;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::ServerState;
use hya_api::error::{ApiError, Code};

/// Directory scope header (D6): overrides the `directory` request field.
pub(crate) const DIRECTORY_HEADER: &str = "x-hya-directory";

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .merge(process::router())
        .merge(catalog::router())
        .merge(agent_models::router())
        .merge(auth::router())
        .merge(logs::router())
        .merge(session::router())
        .merge(bundle_api::router())
        .merge(turn::router())
        .merge(message::router())
        .merge(events::router())
        .merge(interaction::router())
        .merge(workflow::router())
        .merge(fs::router())
        .merge(project::router())
        .merge(worktree::router())
        .merge(mcp::router())
        .merge(pty::router())
}

/// One failed v1 call rendered as `{"error": {"code", "message"}}` with the
/// canonical HTTP status from the stable error table.
pub(crate) struct V1Error(ApiError);

impl V1Error {
    /// Build an error from a stable code and message.
    pub(crate) fn new(code: Code, message: impl Into<String>) -> Self {
        Self(ApiError::new(code, message))
    }

    /// The request body or parameters are invalid.
    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(Code::InvalidArgument, message)
    }

    /// The session id does not exist.
    pub(crate) fn session_not_found(session: &str) -> Self {
        Self::new(
            Code::SessionNotFound,
            format!("session not found: {session}"),
        )
    }

    /// Another run owns the session's admission slot.
    pub(crate) fn session_busy() -> Self {
        Self::new(Code::SessionBusy, "session busy")
    }

    /// A required runtime capability is unavailable.
    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(Code::Unavailable, message)
    }

    /// Unhandled internal failure.
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(Code::Internal, message)
    }

    /// The same failure as a gRPC status (stable code table mapping).
    pub(crate) fn grpc_status(&self) -> tonic::Status {
        self.0.grpc_status()
    }
}

impl From<crate::ApiError> for V1Error {
    fn from(error: crate::ApiError) -> Self {
        let code = match error.code() {
            Some("invalid_argument") | Some("bad_request") => Code::InvalidArgument,
            Some("session_not_found") | Some("not_found") => Code::SessionNotFound,
            Some("session_busy") | Some("conflict") => Code::SessionBusy,
            Some("forbidden") | Some("permission_denied") => Code::PermissionDenied,
            Some("unavailable") | Some("service_unavailable") => Code::Unavailable,
            _ => Code::Internal,
        };
        Self::new(code, error.text().to_owned())
    }
}

impl From<hya_core::CoreError> for V1Error {
    fn from(error: hya_core::CoreError) -> Self {
        Self::new(Code::Internal, error.to_string())
    }
}

impl From<hya_store::StoreError> for V1Error {
    fn from(error: hya_store::StoreError) -> Self {
        Self::new(Code::Internal, error.to_string())
    }
}

impl IntoResponse for V1Error {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.0.code().http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (
            status,
            Json(json!({
                "error": {
                    "code": self.0.code().as_str(),
                    "message": self.0.message(),
                }
            })),
        )
            .into_response()
    }
}

/// Resolve the directory scope for a request.
///
/// Precedence: the `x-hya-directory` header, then the request's
/// `directory` field, then the process workdir.
pub(crate) fn scope_directory(
    headers: &axum::http::HeaderMap,
    requested: &str,
) -> std::path::PathBuf {
    if let Some(header) = headers
        .get(DIRECTORY_HEADER)
        .and_then(|value| value.to_str().ok())
        && !header.trim().is_empty()
    {
        return std::path::PathBuf::from(header.trim());
    }
    if !requested.trim().is_empty() {
        return std::path::PathBuf::from(requested.trim());
    }
    std::path::PathBuf::from(".")
}

/// Build a generated request message from path variables and query
/// parameters.
///
/// GET/DELETE requests carry their fields as query parameters in protojson
/// camelCase form; path variables override query values. String-encoded
/// numbers follow protojson rules.
pub(crate) fn query_request<T: DeserializeOwned>(
    path_vars: &[(&str, &str)],
    query: &BTreeMap<String, String>,
) -> Result<T, V1Error> {
    let mut map = serde_json::Map::new();
    for (key, value) in query {
        if value.is_empty() {
            continue;
        }
        // Query strings carry every value as text; protojson bools need
        // real JSON booleans, so coerce the canonical literals.
        let json = match value.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::String(value.clone()),
        };
        map.insert(key.clone(), json);
    }
    for (key, value) in path_vars {
        map.insert((*key).to_owned(), Value::String((*value).to_owned()));
    }
    serde_json::from_value(Value::Object(map))
        .map_err(|error| V1Error::invalid_argument(format!("invalid request fields: {error}")))
}

pub use grpc::V1Grpc;
