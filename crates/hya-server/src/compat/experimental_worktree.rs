use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;

use crate::ServerState;

use super::{location, worktree_git};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route(
            "/experimental/worktree",
            get(list).post(create).delete(remove),
        )
        .route("/experimental/worktree/reset", post(reset))
}

#[derive(Debug)]
struct WorktreeError {
    name: &'static str,
    message: String,
}

#[derive(Serialize)]
struct WorktreeErrorBody {
    name: &'static str,
    data: WorktreeErrorData,
}

#[derive(Serialize)]
struct WorktreeErrorData {
    message: String,
}

impl WorktreeError {
    fn new(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            message: message.into(),
        }
    }
}

impl IntoResponse for WorktreeError {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(WorktreeErrorBody {
                name: self.name,
                data: WorktreeErrorData {
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

async fn list(State(st): State<ServerState>) -> Result<Json<Vec<String>>, WorktreeError> {
    let source = location::workdir(&st);
    worktree_git::list(&source)
        .await
        .map(Json)
        .map_err(|message| WorktreeError::new("WorktreeListFailedError", message))
}

async fn create(
    State(st): State<ServerState>,
    body: Bytes,
) -> Result<Json<worktree_git::Info>, WorktreeError> {
    let payload = parse_object_body(&body, "WorktreeCreateFailedError")?;
    let name = optional_string(&payload, "name", "WorktreeCreateFailedError")?;
    let source = location::workdir(&st);
    worktree_git::create(&source, name.as_deref())
        .await
        .map(Json)
        .map_err(|message| WorktreeError::new("WorktreeCreateFailedError", message))
}

async fn remove(State(st): State<ServerState>, body: Bytes) -> Result<Json<bool>, WorktreeError> {
    let payload = parse_object_body(&body, "WorktreeRemoveFailedError")?;
    let directory = required_string(&payload, "directory", "WorktreeRemoveFailedError")?;
    let source = location::workdir(&st);
    worktree_git::remove(&source, &directory)
        .await
        .map(Json)
        .map_err(|message| WorktreeError::new("WorktreeRemoveFailedError", message))
}

async fn reset(State(st): State<ServerState>, body: Bytes) -> Result<Json<bool>, WorktreeError> {
    let payload = parse_object_body(&body, "WorktreeResetFailedError")?;
    let directory = required_string(&payload, "directory", "WorktreeResetFailedError")?;
    let source = location::workdir(&st);
    worktree_git::reset(&source, &directory)
        .await
        .map(Json)
        .map_err(|message| WorktreeError::new("WorktreeResetFailedError", message))
}

fn parse_object_body(
    body: &[u8],
    error_name: &'static str,
) -> Result<serde_json::Map<String, Value>, WorktreeError> {
    if body.is_empty() {
        return Ok(serde_json::Map::new());
    }
    let value: Value = serde_json::from_slice(body)
        .map_err(|e| WorktreeError::new(error_name, format!("invalid worktree payload: {e}")))?;
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(WorktreeError::new(
            error_name,
            "worktree payload must be an object",
        )),
    }
}

fn optional_string(
    payload: &serde_json::Map<String, Value>,
    field: &str,
    error_name: &'static str,
) -> Result<Option<String>, WorktreeError> {
    match payload.get(field) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(WorktreeError::new(
            error_name,
            format!("{field} must be a string"),
        )),
        None => Ok(None),
    }
}

fn required_string(
    payload: &serde_json::Map<String, Value>,
    field: &str,
    error_name: &'static str,
) -> Result<String, WorktreeError> {
    optional_string(payload, field, error_name)?
        .ok_or_else(|| WorktreeError::new(error_name, format!("missing worktree {field}")))
}
