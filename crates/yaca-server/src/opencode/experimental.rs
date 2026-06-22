use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::{ApiError, ServerState, parse_session};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/experimental/capabilities", get(capabilities))
        .route("/experimental/console", get(console))
        .route("/experimental/console/orgs", get(console_orgs))
        .route("/experimental/console/switch", post(unavailable))
        .route("/experimental/workspace/adapter", get(empty_array))
        .route(
            "/experimental/workspace",
            get(empty_array).post(unavailable),
        )
        .route("/experimental/workspace/status", get(empty_array))
        .route("/experimental/workspace/sync-list", post(no_content))
        .route("/experimental/workspace/warp", post(unavailable))
        .route("/experimental/workspace/:id", delete(ok_true))
        .route(
            "/experimental/control-plane/move-session",
            post(move_session),
        )
        .route("/experimental/tool", get(empty_array))
        .route("/experimental/tool/ids", get(empty_array))
        .route("/experimental/session", get(session_list))
        .route(
            "/experimental/session/:session/background",
            post(session_background),
        )
        .route("/experimental/resource", get(resource))
        .route("/sync/history", post(empty_array))
        .route("/sync/replay", post(unavailable))
        .route("/sync/steal", post(unavailable))
        .route("/sync/start", post(ok_true))
}

async fn capabilities() -> Json<Value> {
    Json(json!({"backgroundSubagents": false}))
}

async fn console() -> Json<Value> {
    Json(json!({
        "consoleManagedProviders": [],
        "switchableOrgCount": 0
    }))
}

async fn console_orgs() -> Json<Value> {
    Json(json!({"orgs": []}))
}

async fn empty_array() -> Json<Vec<Value>> {
    Json(Vec::new())
}

async fn resource() -> Json<Value> {
    Json(json!({}))
}

async fn no_content() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn ok_true() -> Json<bool> {
    Json(true)
}

async fn unavailable() -> Result<Json<Value>, ApiError> {
    Err(ApiError::bad_request("experimental route is unavailable"))
}

async fn move_session(
    State(st): State<ServerState>,
    Json(payload): Json<Value>,
) -> Result<Response, ApiError> {
    let Some(session_id) = payload.get("sessionID").and_then(Value::as_str) else {
        return Ok(move_session_error("Missing sessionID"));
    };
    let session = match parse_session(session_id) {
        Ok(session) => session,
        Err(_) => {
            return Ok(move_session_error(format!(
                "Session not found: {session_id}"
            )));
        }
    };
    let Some(directory) = payload
        .pointer("/destination/directory")
        .and_then(Value::as_str)
    else {
        return Ok(move_session_error("Missing destination directory"));
    };
    if directory.is_empty() {
        return Ok(move_session_error("Missing destination directory"));
    }
    let projection = st
        .engine
        .store()
        .read_projection(session)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if projection.session.id.is_none() {
        return Ok(move_session_error(format!(
            "Session not found: {session_id}"
        )));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn move_session_error(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "name": "MoveSessionError",
            "data": { "message": message.into() },
        })),
    )
        .into_response()
}

async fn session_list(
    State(st): State<ServerState>,
) -> Result<Json<Vec<super::projection::OpenCodeSessionInfo>>, ApiError> {
    let sessions = st
        .engine
        .store()
        .list_sessions()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut out = Vec::new();
    for session in sessions {
        out.push(
            super::load_session(&st, session.session, Some(session.started_millis))
                .await?
                .info,
        );
    }
    Ok(Json(out))
}

async fn session_background(
    State(st): State<ServerState>,
    AxumPath(session): AxumPath<String>,
) -> Result<Json<bool>, ApiError> {
    let session = parse_session(&session)?;
    super::load_session(&st, session, None).await?;
    Ok(Json(false))
}
