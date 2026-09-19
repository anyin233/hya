//! `/v1` project and VCS domain: project registry, git status/diff/apply.

use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::ServerState;
use hya_api::v1 as pb;

use super::{V1Error, scope_directory};

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/projects", get(list_projects))
        .route("/v1/projects/current", get(current_project))
        .route(
            "/v1/projects/:project",
            axum::routing::patch(update_project),
        )
        .route(
            "/v1/projects/:project/directories",
            get(list_project_directories),
        )
        .route("/v1/projects/:project/init-git", post(init_project_git))
        .route("/v1/vcs", get(get_vcs_status))
        .route("/v1/vcs/diff", get(get_vcs_diff))
        .route("/v1/vcs/apply", post(apply_patch))
}

fn project_info(st: &ServerState) -> pb::ProjectInfo {
    let directory = st.agent.workdir.to_string_lossy().into_owned();
    let name = directory.rsplit('/').next().unwrap_or_default().to_owned();
    pb::ProjectInfo {
        id: name.clone(),
        directory,
        name,
    }
}

async fn list_projects(
    State(st): State<ServerState>,
    Query(_query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::ListProjectsResponse>, V1Error> {
    Ok(Json(pb::ListProjectsResponse {
        projects: vec![project_info(&st)],
        page: Some(pb::PageInfo::default()),
    }))
}

async fn current_project(State(st): State<ServerState>) -> Result<Json<pb::ProjectInfo>, V1Error> {
    Ok(Json(project_info(&st)))
}

async fn update_project(
    State(st): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
    Json(_request): Json<pb::UpdateProjectRequest>,
) -> Result<Json<pb::ProjectInfo>, V1Error> {
    // Project metadata persistence is launcher-owned for now; the v1
    // surface reflects the served project as-is.
    Ok(Json(project_info(&st)))
}

async fn list_project_directories(
    State(st): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
) -> Result<Json<pb::ListProjectDirectoriesResponse>, V1Error> {
    Ok(Json(pb::ListProjectDirectoriesResponse {
        directories: vec![st.agent.workdir.to_string_lossy().into_owned()],
    }))
}

async fn init_project_git(
    State(st): State<ServerState>,
    AxumPath(_project): AxumPath<String>,
) -> Result<Json<pb::InitProjectGitResponse>, V1Error> {
    let workdir = PathBuf::from(&st.agent.workdir);
    if crate::compat::instance::vcs::git::is_repo(&workdir) {
        return Ok(Json(pb::InitProjectGitResponse { initialized: false }));
    }
    let output = tokio::process::Command::new("git")
        .arg("init")
        .current_dir(&workdir)
        .output()
        .await
        .map_err(|error| V1Error::internal(error.to_string()))?;
    if !output.status.success() {
        return Err(V1Error::internal(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(Json(pb::InitProjectGitResponse { initialized: true }))
}

async fn get_vcs_status(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::VcsStatus>, V1Error> {
    let request: pb::GetVcsStatusRequest = super::query_request(&[], &query)?;
    let workdir = scope_directory(&headers, &request.directory);
    let branch = crate::compat::instance::vcs::git::branch(&workdir);
    let head = tokio::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&workdir)
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    let files = if crate::compat::instance::vcs::git::is_repo(&workdir) {
        crate::compat::instance::vcs::git::status(&workdir).map_err(V1Error::from)?
    } else {
        Vec::new()
    };
    let dirty = files.len() as u32;
    let mapped = files
        .iter()
        .map(|file| pb::VcsFileChange {
            path: serde_json::to_value(file)
                .ok()
                .and_then(|value| {
                    value
                        .get("path")
                        .or_else(|| value.get("file"))
                        .and_then(|v| v.as_str().map(str::to_owned))
                })
                .unwrap_or_default(),
            status: file_status(file),
        })
        .collect();
    Ok(Json(pb::VcsStatus {
        branch: branch.unwrap_or_default(),
        head,
        dirty,
        ahead: 0,
        behind: 0,
        files: mapped,
    }))
}

fn file_status(file: &crate::compat::instance::vcs::git::FileStatus) -> i32 {
    let value = serde_json::to_value(file).unwrap_or(serde_json::Value::Null);
    match value
        .get("status")
        .or_else(|| value.get("state"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
    {
        "added" => pb::VcsFileStatus::Added as i32,
        "deleted" => pb::VcsFileStatus::Deleted as i32,
        "renamed" => pb::VcsFileStatus::Renamed as i32,
        "untracked" => pb::VcsFileStatus::Untracked as i32,
        _ => pb::VcsFileStatus::Modified as i32,
    }
}

async fn get_vcs_diff(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::GetVcsDiffResponse>, V1Error> {
    let request: pb::GetVcsDiffRequest = super::query_request(&[], &query)?;
    let workdir = scope_directory(&headers, &request.directory);
    let diff = if crate::compat::instance::vcs::git::is_repo(&workdir) {
        crate::compat::instance::vcs::git::raw_diff(&workdir).map_err(V1Error::from)?
    } else {
        String::new()
    };
    Ok(Json(pb::GetVcsDiffResponse { diff }))
}

async fn apply_patch(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    Json(request): Json<pb::ApplyPatchRequest>,
) -> Result<Json<pb::ApplyPatchResponse>, V1Error> {
    let scope: pb::GetVcsStatusRequest = super::query_request(&[], &query)?;
    let workdir = scope_directory(&headers, &scope.directory);
    let _ = &request.directory;
    if !crate::compat::instance::vcs::git::is_repo(&workdir) {
        return Err(V1Error::invalid_argument(
            "patch cannot be applied: the directory is not a git repository",
        ));
    }
    match crate::compat::instance::vcs::git::apply_patch(&workdir, &request.patch) {
        Ok(()) => Ok(Json(pb::ApplyPatchResponse {
            applied: true,
            summary: String::new(),
        })),
        Err(_) => Err(V1Error::new(
            hya_api::error::Code::Conflict,
            "patch cannot be applied to the current working tree",
        )),
    }
}
