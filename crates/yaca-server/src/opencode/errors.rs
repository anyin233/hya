use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use yaca_proto::SessionId;

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
