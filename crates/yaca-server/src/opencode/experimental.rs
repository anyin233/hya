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
        .route("/experimental/workspace/warp", post(workspace_warp))
        .route("/experimental/workspace/:id", delete(workspace_remove))
        .route(
            "/experimental/control-plane/move-session",
            post(move_session),
        )
        .route("/experimental/tool", get(tool_list))
        .route("/experimental/tool/ids", get(tool_ids))
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
        .route("/sync/replay", post(sync_replay))
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

async fn empty_array() -> Json<Vec<Value>> {
    Json(Vec::new())
}

async fn resource() -> Json<Value> {
    Json(json!({}))
}

async fn tool_list(State(st): State<ServerState>) -> Json<Vec<Value>> {
    let mut schemas = st.engine.tool_schemas();
    schemas.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    Json(
        schemas
            .into_iter()
            .map(|schema| {
                json!({
                    "id": schema.name.to_string(),
                    "description": schema.description,
                    "parameters": schema.input_schema,
                })
            })
            .collect(),
    )
}

async fn tool_ids(State(st): State<ServerState>) -> Json<Vec<String>> {
    let mut ids: Vec<_> = st
        .engine
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.to_string())
        .collect();
    ids.sort();
    Json(ids)
}

async fn no_content() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn ok_true() -> Json<bool> {
    Json(true)
}

async fn workspace_remove() -> Json<Value> {
    Json(Value::Null)
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
    match payload.get("id") {
        Some(Value::Null) => {}
        Some(Value::String(id)) => {
            return Ok(not_found_error(format!("Workspace not found: {id}")));
        }
        _ => return Ok(workspace_warp_error("Workspace warp is unavailable")),
    }
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
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn sync_replay(Json(payload): Json<Value>) -> Result<Json<Value>, ApiError> {
    if payload.get("directory").and_then(Value::as_str).is_none() {
        return Err(ApiError::bad_request("sync replay missing directory"));
    }
    let Some(first) = payload
        .get("events")
        .and_then(Value::as_array)
        .and_then(|events| events.first())
    else {
        return Err(ApiError::bad_request("sync replay requires events"));
    };
    let Some(session_id) = first.get("aggregateID").and_then(Value::as_str) else {
        return Err(ApiError::bad_request(
            "sync replay event missing aggregateID",
        ));
    };
    Ok(Json(json!({ "sessionID": session_id })))
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
