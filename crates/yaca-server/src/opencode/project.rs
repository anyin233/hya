use std::process::Command as StdCommand;

use axum::extract::{Path as AxumPath, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tokio::process::Command;
use yaca_proto::now_millis;

use crate::{ApiError, ServerState};

use super::location;

const PROJECT_ID: &str = "global";

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/project", get(list))
        .route("/project/current", get(current))
        .route("/project/git/init", post(init_git))
        .route("/project/:project/directories", get(directories))
}

#[derive(Serialize)]
struct ProjectInfo {
    id: &'static str,
    worktree: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    vcs: Option<&'static str>,
    time: ProjectTime,
    sandboxes: Vec<String>,
}

#[derive(Serialize)]
struct ProjectTime {
    created: u64,
    updated: u64,
}

#[derive(Serialize)]
struct ProjectDirectory {
    directory: String,
}

async fn list(State(st): State<ServerState>) -> Json<Vec<ProjectInfo>> {
    Json(vec![project_info(&st)])
}

async fn current(State(st): State<ServerState>) -> Json<ProjectInfo> {
    Json(project_info(&st))
}

async fn init_git(State(st): State<ServerState>) -> Result<Json<ProjectInfo>, ApiError> {
    let worktree = location::workdir(&st);
    if !is_git_worktree(&worktree) {
        let output = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&worktree)
            .output()
            .await
            .map_err(|e| ApiError::internal(format!("git spawn failed: {e}")))?;
        if !output.status.success() {
            return Err(ApiError::internal(git_error("git init failed", &output)));
        }
    }
    Ok(Json(project_info(&st)))
}

async fn directories(
    State(st): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
) -> Json<Vec<ProjectDirectory>> {
    Json(vec![ProjectDirectory {
        directory: location::workdir(&st).to_string_lossy().into_owned(),
    }])
}

fn project_info(st: &ServerState) -> ProjectInfo {
    let worktree = location::workdir(st);
    let now = u64::try_from(now_millis()).unwrap_or(0);
    ProjectInfo {
        id: PROJECT_ID,
        worktree: worktree.to_string_lossy().into_owned(),
        vcs: is_git_worktree(&worktree).then_some("git"),
        time: ProjectTime {
            created: now,
            updated: now,
        },
        sandboxes: Vec::new(),
    }
}

fn is_git_worktree(worktree: &std::path::Path) -> bool {
    StdCommand::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(worktree)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git_error(prefix: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = stderr.trim();
    if !detail.is_empty() {
        return format!("{prefix}: {detail}");
    }
    let detail = stdout.trim();
    if !detail.is_empty() {
        return format!("{prefix}: {detail}");
    }
    prefix.to_string()
}
