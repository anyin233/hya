use std::path::Path;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, body::Bytes};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ApiError, ServerState, WorkspaceAdapterInfo};

use super::{location, worktree_git};

pub(super) async fn adapters(State(st): State<ServerState>) -> Json<Vec<WorkspaceAdapterInfo>> {
    let mut adapters = vec![WorkspaceAdapterInfo {
        r#type: "worktree".to_string(),
        name: "Worktree".to_string(),
        description: "Create a git worktree".to_string(),
    }];
    adapters.extend(
        st.workspace_adapters
            .into_iter()
            .filter(|adapter| adapter.r#type != "worktree"),
    );
    Json(adapters)
}

pub(super) async fn list(State(st): State<ServerState>) -> Result<Json<Vec<Info>>, ApiError> {
    workspace_list(&st).await.map(Json)
}

pub(super) async fn find(st: &ServerState, id: &str) -> Result<Option<Info>, ApiError> {
    Ok(workspace_list(st)
        .await?
        .into_iter()
        .find(|workspace| workspace.id == id))
}

pub(super) async fn status() -> Json<Vec<Value>> {
    Json(Vec::new())
}

pub(super) async fn sync_list() -> StatusCode {
    StatusCode::NO_CONTENT
}

pub(super) async fn create(
    State(st): State<ServerState>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let payload = parse_create(&body)?;
    if payload.r#type != "worktree" {
        return Ok(workspace_error(
            "WorkspaceCreateError",
            format!("Workspace adapter not found: {}", payload.r#type),
        ));
    }
    let source = location::workdir(&st);
    let info = match worktree_git::create(&source, payload.name.as_deref()).await {
        Ok(info) => info,
        Err(message) => return Ok(workspace_error("WorkspaceCreateError", message)),
    };
    Ok(Json(workspace_info(&source, &info)).into_response())
}

pub(super) async fn remove(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Response, ApiError> {
    let source = location::workdir(&st);
    let Some(workspace) = workspace_list(&st)
        .await?
        .into_iter()
        .find(|workspace| workspace.id == id)
    else {
        return Ok(Json(Value::Null).into_response());
    };
    worktree_git::remove(&source, &workspace.directory)
        .await
        .map_err(ApiError::bad_request)?;
    Ok(Json(workspace).into_response())
}

#[derive(Debug, Deserialize)]
struct CreatePayload {
    #[serde(rename = "type")]
    r#type: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct Info {
    id: String,
    #[serde(rename = "type")]
    r#type: &'static str,
    name: String,
    branch: Option<String>,
    directory: String,
    #[serde(rename = "projectID")]
    project_id: String,
    #[serde(rename = "timeUsed")]
    time_used: u64,
}

impl Info {
    pub(super) fn directory(&self) -> &str {
        &self.directory
    }
}

async fn workspace_list(st: &ServerState) -> Result<Vec<Info>, ApiError> {
    let source = location::workdir(st);
    let infos = worktree_git::infos(&source)
        .await
        .map_err(ApiError::bad_request)?;
    Ok(infos
        .into_iter()
        .map(|info| workspace_info(&source, &info))
        .collect())
}

fn workspace_info(source: &Path, info: &worktree_git::Info) -> Info {
    Info {
        id: stable_id("wrk", info.directory()),
        r#type: "worktree",
        name: info.name().to_string(),
        branch: info.branch().map(ToString::to_string),
        directory: info.directory().to_string(),
        project_id: stable_id("proj", &source.to_string_lossy()),
        time_used: 0,
    }
}

fn parse_create(body: &[u8]) -> Result<CreatePayload, ApiError> {
    if body.is_empty() {
        return Err(ApiError::bad_request("workspace payload is required"));
    }
    serde_json::from_slice(body).map_err(|error| ApiError::bad_request(error.to_string()))
}

fn workspace_error(name: &'static str, message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "name": name,
            "data": { "message": message.into() },
        })),
    )
        .into_response()
}

fn stable_id(prefix: &str, text: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{prefix}_{hash:016x}")
}
