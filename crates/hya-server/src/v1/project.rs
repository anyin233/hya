//! `/v1` project and VCS domain: the Project registry (ADR-0024) over the
//! store's `project` tables, and git status/diff/apply.

use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};

use super::Json;

use crate::ServerState;
use hya_api::v1 as pb;
use hya_proto::ProjectId;
use hya_store::Project;

use super::{V1Error, scope_directory};

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/projects", get(list_projects).post(create_project))
        .route("/v1/projects/current", get(current_project))
        .route("/v1/projects/resolve", get(resolve_project))
        .route("/v1/projects/ensure", post(ensure_project))
        .route(
            "/v1/projects/:project",
            get(get_project)
                .patch(update_project)
                .delete(delete_project),
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

/// Parse a wire Project id; a malformed one is `invalid_argument`.
pub(crate) fn parse_project(id: &str) -> Result<ProjectId, V1Error> {
    id.parse::<ProjectId>()
        .map_err(|_| V1Error::invalid_argument(format!("invalid project id: {id}")))
}

/// Load one Project; `not_found` when it does not exist.
pub(crate) async fn load_project(st: &ServerState, id: ProjectId) -> Result<Project, V1Error> {
    st.engine
        .store()
        .get_project(id)
        .await?
        .ok_or_else(|| V1Error::not_found(format!("project not found: {id}")))
}

/// Whether a non-archived session of the Project is running a turn now
/// (`ServerState::is_busy`, the source of `SessionInfo.busy`).
pub(crate) async fn project_busy(st: &ServerState, id: ProjectId) -> Result<bool, V1Error> {
    for row in st.engine.store().list_sessions_in(Some(id)).await? {
        if st.is_busy(row.session)
            && !st
                .engine
                .read_projection_shared(row.session)
                .await?
                .session
                .is_archived()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Wire view of a Project; reads its session count unless given.
async fn project_info(
    st: &ServerState,
    project: &Project,
    session_count: Option<u64>,
) -> Result<pb::ProjectInfo, V1Error> {
    let session_count = match session_count {
        Some(count) => count,
        None => st.engine.store().project_session_count(project.id).await?,
    };
    Ok(pb::ProjectInfo {
        id: project.id.to_string(),
        name: project.name.clone(),
        roots: project.roots.clone(),
        created_at: super::convert::timestamp(project.created_at_ms),
        updated_at: super::convert::timestamp(project.updated_at_ms),
        session_count: u32::try_from(session_count).unwrap_or(u32::MAX),
        busy: project_busy(st, project.id).await?,
    })
}

/// `EnsureProjectForPath`: the Project whose root contains `path` (longest
/// root wins), else a new one named after `path`'s last component with
/// `path` as its only root. Returns the Project and whether it was created.
/// [`hya_store::SessionStore::ensure_project_for_path`] resolves and creates
/// in one `BEGIN IMMEDIATE` transaction, so concurrent callers for one
/// directory always converge on exactly one Project without a process-wide
/// lock here.
pub(crate) async fn ensure_project_for_path(
    st: &ServerState,
    path: &str,
) -> Result<(Project, bool), V1Error> {
    let (project, created) = st.engine.store().ensure_project_for_path(path).await?;
    if created {
        st.notify_projects_updated();
    }
    Ok((project, created))
}

async fn list_projects(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::ListProjectsResponse>, V1Error> {
    let request: pb::ListProjectsRequest = super::query_request(&[], &query)?;
    let summaries = st.engine.store().list_projects().await?;
    let mut infos = Vec::with_capacity(summaries.len());
    for summary in &summaries {
        infos.push(project_info(&st, &summary.project, Some(summary.session_count)).await?);
    }
    let (projects, page) = super::catalog::paginate(infos, &request.page);
    Ok(Json(pb::ListProjectsResponse {
        projects,
        page: Some(page),
    }))
}

async fn current_project(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ProjectInfo>, V1Error> {
    let request: pb::GetCurrentProjectRequest = super::query_request(&[], &query)?;
    let scope = scope_directory(&headers, &request.directory)?;
    let scope = scope.to_string_lossy();
    let project = st
        .engine
        .store()
        .resolve_project_by_path(&scope)
        .await?
        .ok_or_else(|| V1Error::not_found(format!("no project contains {scope}")))?;
    Ok(Json(project_info(&st, &project, None).await?))
}

async fn resolve_project(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::ResolveProjectResponse>, V1Error> {
    let request: pb::ResolveProjectRequest = super::query_request(&[], &query)?;
    let project = match st
        .engine
        .store()
        .resolve_project_by_path(request.path.trim())
        .await?
    {
        Some(project) => Some(project_info(&st, &project, None).await?),
        None => None,
    };
    Ok(Json(pb::ResolveProjectResponse { project }))
}

async fn ensure_project(
    State(st): State<ServerState>,
    Json(request): Json<pb::EnsureProjectForPathRequest>,
) -> Result<Json<pb::EnsureProjectForPathResponse>, V1Error> {
    let (project, created) = ensure_project_for_path(&st, &request.path).await?;
    Ok(Json(pb::EnsureProjectForPathResponse {
        project: Some(project_info(&st, &project, None).await?),
        created,
    }))
}

async fn create_project(
    State(st): State<ServerState>,
    Json(request): Json<pb::CreateProjectRequest>,
) -> Result<Json<pb::ProjectInfo>, V1Error> {
    let project = st
        .engine
        .store()
        .create_project(&request.name, &request.roots)
        .await?;
    st.notify_projects_updated();
    Ok(Json(project_info(&st, &project, Some(0)).await?))
}

async fn get_project(
    State(st): State<ServerState>,
    AxumPath(project): AxumPath<String>,
) -> Result<Json<pb::ProjectInfo>, V1Error> {
    let project = load_project(&st, parse_project(&project)?).await?;
    Ok(Json(project_info(&st, &project, None).await?))
}

async fn update_project(
    State(st): State<ServerState>,
    AxumPath(project): AxumPath<String>,
    Json(request): Json<pb::UpdateProjectRequest>,
) -> Result<Json<pb::ProjectInfo>, V1Error> {
    let id = parse_project(&project)?;
    // Empty `roots` keeps the current roots (a Project always has one).
    let roots = (!request.roots.is_empty()).then_some(request.roots.as_slice());
    let previous_roots = match roots {
        Some(_) => Some(load_project(&st, id).await?.roots),
        None => None,
    };
    let project = st
        .engine
        .store()
        .update_project(id, request.name.as_deref(), roots)
        .await?;
    if request.name.is_some() || roots.is_some() {
        st.notify_projects_updated();
    }
    // New roots change the Project's catalog (skills, commands, bundles,
    // plugins): drop its scope, which emits one `catalogUpdated {projectId}`.
    if previous_roots.is_some_and(|previous| previous != project.roots) {
        st.engine.invalidate_catalog_scope(id);
    }
    Ok(Json(project_info(&st, &project, None).await?))
}

async fn delete_project(
    State(st): State<ServerState>,
    AxumPath(project): AxumPath<String>,
) -> Result<Json<pb::DeleteProjectResponse>, V1Error> {
    let id = parse_project(&project)?;
    if !st.engine.store().delete_project(id).await? {
        return Err(V1Error::not_found(format!("project not found: {id}")));
    }
    st.notify_projects_updated();
    // Its directories fall back to plain-directory catalogs.
    st.engine.invalidate_catalog_scope(id);
    Ok(Json(pb::DeleteProjectResponse {}))
}

async fn list_project_directories(
    State(st): State<ServerState>,
    AxumPath(project): AxumPath<String>,
) -> Result<Json<pb::ListProjectDirectoriesResponse>, V1Error> {
    let project = load_project(&st, parse_project(&project)?).await?;
    Ok(Json(pb::ListProjectDirectoriesResponse {
        directories: project.roots,
    }))
}

async fn init_project_git(
    State(st): State<ServerState>,
    AxumPath(project): AxumPath<String>,
) -> Result<Json<pb::InitProjectGitResponse>, V1Error> {
    let project = load_project(&st, parse_project(&project)?).await?;
    let Some(workdir) = project.roots.first().map(PathBuf::from) else {
        return Err(V1Error::internal("project has no roots"));
    };
    if crate::support::git::is_repo(&workdir) {
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
    let workdir = scope_directory(&headers, &request.directory)?;
    let branch = crate::support::git::branch(&workdir);
    let head = tokio::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&workdir)
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default();
    let files = if crate::support::git::is_repo(&workdir) {
        crate::support::git::status(&workdir).map_err(V1Error::from)?
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

fn file_status(file: &crate::support::git::FileStatus) -> i32 {
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
    Query(query): Query<Vec<(String, String)>>,
    headers: HeaderMap,
) -> Result<Json<pb::GetVcsDiffResponse>, V1Error> {
    let request: pb::GetVcsDiffRequest =
        super::query_request_pairs(&[], query.iter().map(|(k, v)| (k, v)), &["paths"])?;
    let workdir = scope_directory(&headers, &request.directory)?;
    let paths = diff_paths(&request.paths)?;
    // `raw` is accepted and ignored: the diff is always git's unified patch.
    let diff = if crate::support::git::is_repo(&workdir) {
        crate::support::git::raw_diff(&workdir, &paths).map_err(V1Error::from)?
    } else {
        String::new()
    };
    Ok(Json(pb::GetVcsDiffResponse { diff }))
}

/// Validate `GetVcsDiff.paths`: relative paths inside the scope directory.
/// Empty entries are dropped; an empty result diffs everything.
fn diff_paths(paths: &[String]) -> Result<Vec<String>, V1Error> {
    let mut out = Vec::new();
    for path in paths
        .iter()
        .map(|path| path.trim())
        .filter(|p| !p.is_empty())
    {
        let escapes = std::path::Path::new(path).components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        });
        if escapes {
            return Err(V1Error::invalid_argument(format!(
                "diff path must be relative to the repository: {path}"
            )));
        }
        out.push(path.to_owned());
    }
    Ok(out)
}

async fn apply_patch(
    State(_st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    Json(request): Json<pb::ApplyPatchRequest>,
) -> Result<Json<pb::ApplyPatchResponse>, V1Error> {
    let scope: pb::GetVcsStatusRequest = super::query_request(&[], &query)?;
    let workdir = scope_directory(&headers, &scope.directory)?;
    let _ = &request.directory;
    if !crate::support::git::is_repo(&workdir) {
        return Err(V1Error::invalid_argument(
            "patch cannot be applied: the directory is not a git repository",
        ));
    }
    match crate::support::git::apply_patch(&workdir, &request.patch) {
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
