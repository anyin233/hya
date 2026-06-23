use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::ServerState;

use super::location;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route(
            "/experimental/project/:project/copy",
            post(create_copy).delete(remove_copy),
        )
        .route(
            "/experimental/project/:project/copy/refresh",
            post(refresh_copies),
        )
        .route(
            "/experimental/project/:project/copy/generate-name",
            post(generate_name),
        )
}

#[derive(Deserialize)]
struct CreatePayload {
    strategy: String,
    directory: String,
    name: Option<String>,
}

#[derive(Serialize)]
struct CopyResponse {
    directory: String,
}

#[derive(Deserialize)]
struct GenerateNamePayload {
    context: Option<String>,
}

#[derive(Serialize)]
struct GenerateNameResponse {
    name: String,
}

#[derive(Deserialize)]
struct RemovePayload {
    directory: String,
    force: bool,
}

#[derive(Debug)]
struct ProjectCopyError {
    message: String,
    force_required: Option<bool>,
}

#[derive(Serialize)]
struct ProjectCopyErrorBody {
    name: &'static str,
    data: ProjectCopyErrorData,
}

#[derive(Serialize)]
struct ProjectCopyErrorData {
    message: String,
    #[serde(rename = "forceRequired", skip_serializing_if = "Option::is_none")]
    force_required: Option<bool>,
}

impl ProjectCopyError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            force_required: None,
        }
    }

    fn force_required(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            force_required: Some(true),
        }
    }
}

impl IntoResponse for ProjectCopyError {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(ProjectCopyErrorBody {
                name: "ProjectCopyError",
                data: ProjectCopyErrorData {
                    message: self.message,
                    force_required: self.force_required,
                },
            }),
        )
            .into_response()
    }
}

async fn create_copy(
    State(st): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
    Json(payload): Json<CreatePayload>,
) -> Result<Json<CopyResponse>, ProjectCopyError> {
    if payload.strategy != "git_worktree" {
        return Err(ProjectCopyError::new(format!(
            "Project copy strategy unavailable: {}",
            payload.strategy
        )));
    }
    let source = location::workdir(&st);
    ensure_git_source(&source).await?;
    let destination = copy_destination(&payload);
    if destination.exists() {
        return Err(ProjectCopyError::new(format!(
            "Project copy destination already exists: {}",
            destination.to_string_lossy()
        )));
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ProjectCopyError::new(format!(
                "Project copy directory unavailable: {} ({e})",
                parent.to_string_lossy()
            ))
        })?;
    }
    let destination_text = destination.to_string_lossy().into_owned();
    run_git(
        &source,
        vec![
            "worktree".to_string(),
            "add".to_string(),
            "--detach".to_string(),
            destination_text.clone(),
            "HEAD".to_string(),
        ],
    )
    .await?;
    Ok(Json(CopyResponse {
        directory: destination_text,
    }))
}

async fn remove_copy(
    State(st): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
    Json(payload): Json<RemovePayload>,
) -> Result<StatusCode, ProjectCopyError> {
    let source = location::workdir(&st);
    ensure_git_source(&source).await?;
    let directory = PathBuf::from(&payload.directory);
    if !payload.force && is_dirty(&directory).await? {
        return Err(ProjectCopyError::force_required(format!(
            "Project copy directory unavailable: {}",
            directory.to_string_lossy()
        )));
    }
    run_git(
        &source,
        vec![
            "worktree".to_string(),
            "remove".to_string(),
            "--force".to_string(),
            payload.directory,
        ],
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn refresh_copies(
    State(st): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
) -> Result<StatusCode, ProjectCopyError> {
    ensure_git_source(&location::workdir(&st)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn generate_name(
    AxumPath(_project): AxumPath<String>,
    Json(payload): Json<GenerateNamePayload>,
) -> Json<GenerateNameResponse> {
    Json(GenerateNameResponse {
        name: generated_copy_name(payload.context.as_deref()),
    })
}

fn copy_destination(payload: &CreatePayload) -> PathBuf {
    let directory = PathBuf::from(&payload.directory);
    payload
        .name
        .as_ref()
        .map_or(directory.clone(), |name| directory.join(name))
}

fn generated_copy_name(context: Option<&str>) -> String {
    if let Some(text) = context.map(str::trim).filter(|text| !text.is_empty()) {
        let words = text
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ");
        let slug = slugify(&words);
        if !slug.is_empty() {
            return slug;
        }
    }
    fallback_copy_name()
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    out
}

fn fallback_copy_name() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    format!("copy-{millis}-{}", std::process::id())
}

async fn ensure_git_source(source: &Path) -> Result<(), ProjectCopyError> {
    if !source.exists() {
        return Err(ProjectCopyError::new(format!(
            "Project copy source not found: {}",
            source.to_string_lossy()
        )));
    }
    run_git(
        source,
        vec!["rev-parse".to_string(), "--is-inside-work-tree".to_string()],
    )
    .await
    .map(|_| ())
}

async fn is_dirty(directory: &Path) -> Result<bool, ProjectCopyError> {
    if !directory.exists() {
        return Ok(false);
    }
    let output = run_git(
        directory,
        vec!["status".to_string(), "--porcelain".to_string()],
    )
    .await?;
    Ok(!output.trim().is_empty())
}

async fn run_git(cwd: &Path, args: Vec<String>) -> Result<String, ProjectCopyError> {
    let output = Command::new("git")
        .args(&args)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|e| ProjectCopyError::new(format!("git spawn failed: {e}")))?;
    if !output.status.success() {
        return Err(ProjectCopyError::new(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
