use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use hya_proto::SessionId;
use serde_json::json;

pub(in crate::opencode) fn session_not_found(session_id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "_tag": "SessionNotFoundError",
            "sessionID": session_id,
            "message": format!("Session not found: {session_id}"),
        })),
    )
        .into_response()
}

pub(in crate::opencode) fn session_busy(session: SessionId) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "name": "SessionBusyError",
            "data": {
                "sessionID": session.to_string(),
                "message": format!("Session is busy: {session}"),
            },
        })),
    )
        .into_response()
}

pub(in crate::opencode) fn service_unavailable(operation: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "_tag": "ServiceUnavailableError",
            "message": format!("Session {operation} is not available yet"),
            "service": format!("session.{operation}"),
        })),
    )
        .into_response()
}

pub(in crate::opencode) fn legacy_session_not_found(session: SessionId) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "name": "NotFoundError",
            "data": { "message": format!("Session not found: {session}") },
        })),
    )
        .into_response()
}

pub(in crate::opencode) fn legacy_bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "name": "BadRequest",
            "data": { "message": message.into() },
        })),
    )
        .into_response()
}

pub(in crate::opencode) fn invalid_cursor(message: &'static str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "_tag": "InvalidCursorError",
            "message": message,
        })),
    )
        .into_response()
}

pub(in crate::opencode) fn invalid_workspace_query() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "_tag": "InvalidRequestError",
            "message": "Invalid workspace query parameter",
            "kind": "Query",
            "field": "workspace",
        })),
    )
        .into_response()
}
