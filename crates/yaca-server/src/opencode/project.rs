use std::process::Command;

use axum::extract::{Path as AxumPath, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use yaca_proto::now_millis;

use crate::ServerState;

use super::location;

const PROJECT_ID: &str = "global";

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/project", get(list))
        .route("/project/current", get(current))
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
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(worktree)
        .output()
        .is_ok_and(|output| output.status.success())
}
