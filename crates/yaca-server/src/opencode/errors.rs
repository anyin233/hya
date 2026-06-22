use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
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
