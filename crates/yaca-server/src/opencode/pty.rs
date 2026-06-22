use std::collections::BTreeSet;
use std::path::Path;

use axum::extract::Path as AxumPath;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;

use crate::{ApiError, ServerState};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/pty/shells", get(shells))
        .route("/api/pty", get(list).post(create))
        .route("/api/pty/:id", get(get_one).put(update).delete(remove))
        .route("/api/pty/:id/connect-token", post(connect_token))
        .route("/api/pty/:id/connect", get(connect))
}

#[derive(Serialize)]
struct ShellItem {
    path: String,
    name: String,
    acceptable: bool,
}

async fn shells() -> Json<Vec<ShellItem>> {
    Json(shell_candidates().into_iter().map(shell_item).collect())
}

async fn list() -> Json<Vec<Value>> {
    Json(Vec::new())
}

async fn create() -> Result<Json<Value>, ApiError> {
    Err(ApiError::service_unavailable(
        "PTY create is not available yet",
    ))
}

async fn get_one(AxumPath(id): AxumPath<String>) -> Result<Json<Value>, ApiError> {
    Err(pty_not_found(&id))
}

async fn update(AxumPath(id): AxumPath<String>) -> Result<Json<Value>, ApiError> {
    Err(pty_not_found(&id))
}

async fn remove(AxumPath(id): AxumPath<String>) -> Result<Json<Value>, ApiError> {
    Err(pty_not_found(&id))
}

async fn connect_token(AxumPath(id): AxumPath<String>) -> Result<Json<Value>, ApiError> {
    Err(pty_not_found(&id))
}

async fn connect(AxumPath(id): AxumPath<String>) -> Result<Json<Value>, ApiError> {
    Err(pty_not_found(&id))
}

fn shell_candidates() -> Vec<String> {
    let mut paths = BTreeSet::new();
    if let Some(shell) = std::env::var_os("SHELL").and_then(|value| value.into_string().ok()) {
        paths.insert(shell);
    }
    for path in [
        "/bin/bash",
        "/usr/bin/bash",
        "/bin/zsh",
        "/usr/bin/zsh",
        "/bin/sh",
        "/usr/bin/sh",
    ] {
        paths.insert(path.to_string());
    }
    paths.into_iter().collect()
}

fn shell_item(path: String) -> ShellItem {
    let acceptable = is_executable(&path);
    let name = Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path.as_str())
        .to_string();
    ShellItem {
        path,
        name,
        acceptable,
    }
}

fn is_executable(path: &str) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn pty_not_found(id: &str) -> ApiError {
    ApiError::not_found(format!("PTY session not found: {id}"))
}
