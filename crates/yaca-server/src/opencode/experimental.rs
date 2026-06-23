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
        .route(
            "/experimental/workspace/adapter",
            get(super::experimental_workspace::adapters),
        )
        .route(
            "/experimental/workspace",
            get(super::experimental_workspace::list).post(super::experimental_workspace::create),
        )
        .route(
            "/experimental/workspace/status",
            get(super::experimental_workspace::status),
        )
        .route(
            "/experimental/workspace/sync-list",
            post(super::experimental_workspace::sync_list),
        )
        .route("/experimental/workspace/warp", post(workspace_warp))
        .route(
            "/experimental/workspace/:id",
            delete(super::experimental_workspace::remove),
        )
        .route(
            "/experimental/control-plane/move-session",
            post(move_session),
        )
        .route("/experimental/tool", get(super::experimental_tool::list))
        .route("/experimental/tool/ids", get(super::experimental_tool::ids))
        .route(
            "/experimental/session",
            get(super::session_list::list_sessions),
        )
        .route(
            "/experimental/session/:session/background",
            post(session_background),
        )
        .route("/experimental/resource", get(resource))
        .route("/sync/history", post(super::experimental_sync::history))
        .route("/sync/replay", post(super::experimental_sync::replay))
        .route("/sync/steal", post(super::experimental_sync::steal))
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

async fn resource(State(st): State<ServerState>) -> Json<Value> {
    Json(json!(st.mcp_http.resources(&st.mcp_manager).await))
}

async fn ok_true() -> Json<bool> {
    Json(true)
}

async fn unavailable() -> Result<Json<Value>, ApiError> {
    Err(ApiError::bad_request("experimental route is unavailable"))
}

async fn workspace_warp(
    State(st): State<ServerState>,
    Json(payload): Json<Value>,
) -> Result<Response, ApiError> {
    let Some(session_id) = payload.get("sessionID").and_then(Value::as_str) else {
        return Ok(workspace_warp_error("Missing sessionID"));
    };
    let session = match parse_session(session_id) {
        Ok(session) => session,
        Err(_) => {
            return Ok(workspace_warp_error(format!(
                "Session not found: {session_id}"
            )));
        }
    };
    let target_directory = match payload.get("id") {
        Some(Value::Null) => None,
        Some(Value::String(id)) => {
            let Some(workspace) = super::experimental_workspace::find(&st, id).await? else {
                return Ok(not_found_error(format!("Workspace not found: {id}")));
            };
            Some(workspace.directory().to_string())
        }
        _ => return Ok(workspace_warp_error("Workspace warp is unavailable")),
    };
    let projection = st
        .engine
        .store()
        .read_projection(session)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if projection.session.id.is_none() {
        return Ok(workspace_warp_error(format!(
            "Session not found: {session_id}"
        )));
    }
    if let Some(directory) = target_directory
        && projection.session.workdir.as_deref() != Some(directory.as_str())
    {
        st.engine
            .set_workdir(session, directory)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
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
    if projection.session.workdir.as_deref() != Some(directory) {
        st.engine
            .set_workdir(session, directory.to_string())
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
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

fn workspace_warp_error(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "name": "WorkspaceWarpError",
            "data": { "message": message.into() },
        })),
    )
        .into_response()
}

fn not_found_error(message: impl Into<String>) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "name": "NotFoundError",
            "data": { "message": message.into() },
        })),
    )
        .into_response()
}

async fn session_background(
    State(st): State<ServerState>,
    AxumPath(session): AxumPath<String>,
) -> Result<Json<bool>, ApiError> {
    let session = parse_session(&session)?;
    super::load_session(&st, session, None).await?;
    Ok(Json(false))
}
