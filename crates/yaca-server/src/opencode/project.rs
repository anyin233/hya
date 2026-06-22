use std::process::Command as StdCommand;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::RwLock;
use yaca_proto::now_millis;

use crate::{ApiError, ServerState};

use super::location;

const PROJECT_ID: &str = "global";

#[derive(Clone)]
pub(crate) struct ProjectState {
    inner: Arc<RwLock<ProjectRuntime>>,
}

struct ProjectRuntime {
    metadata: ProjectMetadata,
    time: ProjectTime,
}

#[derive(Clone, Default)]
struct ProjectMetadata {
    name: Option<String>,
    icon: Option<ProjectIcon>,
    commands: Option<ProjectCommands>,
}

impl ProjectState {
    pub(crate) fn new() -> Self {
        let now = current_time();
        Self {
            inner: Arc::new(RwLock::new(ProjectRuntime {
                metadata: ProjectMetadata::default(),
                time: ProjectTime {
                    created: now,
                    updated: now,
                },
            })),
        }
    }

    async fn snapshot(&self) -> (ProjectMetadata, ProjectTime) {
        let state = self.inner.read().await;
        (state.metadata.clone(), state.time.clone())
    }

    async fn update(&self, payload: UpdatePayload) -> (ProjectMetadata, ProjectTime) {
        let mut state = self.inner.write().await;
        state.metadata.name = payload.name;
        state.metadata.icon = payload.icon;
        state.metadata.commands = payload.commands;
        state.time.updated = current_time();
        (state.metadata.clone(), state.time.clone())
    }
}

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/project", get(list))
        .route("/project/current", get(current))
        .route("/project/git/init", post(init_git))
        .route("/project/:project", patch(update))
        .route("/project/:project/directories", get(directories))
}

#[derive(Serialize, Clone)]
struct ProjectInfo {
    id: &'static str,
    worktree: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    vcs: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<ProjectIcon>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commands: Option<ProjectCommands>,
    time: ProjectTime,
    sandboxes: Vec<String>,
}

#[derive(Serialize, Clone)]
struct ProjectTime {
    created: u64,
    updated: u64,
}

#[derive(Deserialize, Serialize, Clone)]
struct ProjectIcon {
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(rename = "override", skip_serializing_if = "Option::is_none")]
    override_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
struct ProjectCommands {
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<String>,
}

#[derive(Serialize)]
struct ProjectDirectory {
    directory: String,
}

#[derive(Deserialize)]
struct UpdatePayload {
    name: Option<String>,
    icon: Option<ProjectIcon>,
    commands: Option<ProjectCommands>,
}

async fn list(State(st): State<ServerState>) -> Json<Vec<ProjectInfo>> {
    Json(vec![project_info(&st).await])
}

async fn current(State(st): State<ServerState>) -> Json<ProjectInfo> {
    Json(project_info(&st).await)
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
    Ok(Json(project_info(&st).await))
}

async fn update(
    State(st): State<ServerState>,
    AxumPath(project): AxumPath<String>,
    Json(payload): Json<UpdatePayload>,
) -> Result<Json<ProjectInfo>, ApiError> {
    if project != PROJECT_ID {
        return Err(ApiError::not_found(format!("Project not found: {project}")));
    }
    let (metadata, time) = st.project.update(payload).await;
    Ok(Json(project_info_with_state(&st, metadata, time)))
}

async fn directories(
    State(st): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
) -> Json<Vec<ProjectDirectory>> {
    Json(vec![ProjectDirectory {
        directory: location::workdir(&st).to_string_lossy().into_owned(),
    }])
}

async fn project_info(st: &ServerState) -> ProjectInfo {
    let (metadata, time) = st.project.snapshot().await;
    project_info_with_state(st, metadata, time)
}

fn project_info_with_state(
    st: &ServerState,
    metadata: ProjectMetadata,
    time: ProjectTime,
) -> ProjectInfo {
    let worktree = location::workdir(st);
    ProjectInfo {
        id: PROJECT_ID,
        worktree: worktree.to_string_lossy().into_owned(),
        vcs: is_git_worktree(&worktree).then_some("git"),
        name: metadata.name,
        icon: metadata.icon,
        commands: metadata.commands,
        time,
        sandboxes: Vec::new(),
    }
}

fn current_time() -> u64 {
    u64::try_from(now_millis()).unwrap_or(0)
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
