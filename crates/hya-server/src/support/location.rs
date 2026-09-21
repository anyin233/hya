use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::http::HeaderMap;
use serde::Serialize;

use crate::ServerState;

#[derive(Serialize)]
pub(super) struct LocationResponse<T> {
    location: LocationInfo,
    data: T,
}

#[derive(Clone, Serialize)]
pub(super) struct LocationInfo {
    directory: String,
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    workspace_id: Option<String>,
    project: ProjectInfo,
}

#[derive(Clone, Serialize)]
struct ProjectInfo {
    id: &'static str,
    directory: String,
}

#[derive(Default)]
pub(super) struct LocationRef {
    directory: Option<PathBuf>,
    workspace_id: Option<String>,
}

impl LocationRef {
    pub(super) fn from_request(query: &BTreeMap<String, String>, _headers: &HeaderMap) -> Self {
        let directory = query
            .get("directory")
            .or_else(|| query.get("location[directory]"))
            .cloned()
            .map(PathBuf::from);
        let workspace_id = query
            .get("workspace")
            .or_else(|| query.get("location[workspace]"))
            .cloned();
        Self {
            directory,
            workspace_id,
        }
    }
}

pub(super) fn response<T>(st: &ServerState, data: T) -> LocationResponse<T> {
    response_at(st, &LocationRef::default(), data)
}

pub(super) fn response_at<T>(
    st: &ServerState,
    location: &LocationRef,
    data: T,
) -> LocationResponse<T> {
    LocationResponse {
        location: info_at(st, location),
        data,
    }
}

pub(super) fn info_at(st: &ServerState, location: &LocationRef) -> LocationInfo {
    let directory = workdir_at(st, location).to_string_lossy().into_owned();
    LocationInfo {
        directory: directory.clone(),
        workspace_id: location.workspace_id.clone(),
        project: ProjectInfo {
            id: "global",
            directory,
        },
    }
}

pub(super) fn workdir(st: &ServerState) -> PathBuf {
    workdir_at(st, &LocationRef::default())
}

pub(super) fn workdir_at(st: &ServerState, location: &LocationRef) -> PathBuf {
    let path = location
        .directory
        .clone()
        .or_else(|| {
            location.workspace_id.as_deref().and_then(|id| {
                crate::support::worktree_git_lookup::directory_for_id(&st.agent.workdir, id)
            })
        })
        .unwrap_or_else(|| st.agent.workdir.clone());
    canonical_workdir(path)
}

pub(super) fn canonical_workdir(path: impl Into<PathBuf>) -> PathBuf {
    let path = path.into();
    match std::fs::canonicalize(&path) {
        Ok(path) => path,
        Err(_) => absolute_path(&path),
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir().map_or_else(
        |_| PathBuf::from(std::path::MAIN_SEPARATOR.to_string()).join(path),
        |cwd| cwd.join(path),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_workdir_returns_absolute_path_for_existing_relative_directory() {
        let path = canonical_workdir(".");
        let expected = match std::fs::canonicalize(".") {
            Ok(path) => path,
            Err(error) => panic!("failed to canonicalize current directory: {error}"),
        };

        assert!(path.is_absolute());
        assert_eq!(path, expected);
    }

    #[test]
    fn canonical_workdir_absolutizes_missing_relative_directory() {
        let path = canonical_workdir("target/hya-missing-workdir-test-fixture");

        assert!(path.is_absolute());
        assert!(path.ends_with("target/hya-missing-workdir-test-fixture"));
    }
}
