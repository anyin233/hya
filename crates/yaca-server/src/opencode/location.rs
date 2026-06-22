use std::path::PathBuf;

use serde::Serialize;

use crate::ServerState;

#[derive(Serialize)]
pub(super) struct LocationResponse<T> {
    location: LocationInfo,
    data: T,
}

#[derive(Serialize)]
pub(super) struct LocationInfo {
    directory: String,
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    workspace_id: Option<String>,
    project: ProjectInfo,
}

#[derive(Serialize)]
struct ProjectInfo {
    id: &'static str,
    directory: String,
}

pub(super) fn response<T>(st: &ServerState, data: T) -> LocationResponse<T> {
    LocationResponse {
        location: info(st),
        data,
    }
}

pub(super) fn info(st: &ServerState) -> LocationInfo {
    let directory = workdir(st).to_string_lossy().into_owned();
    LocationInfo {
        directory: directory.clone(),
        workspace_id: None,
        project: ProjectInfo {
            id: "global",
            directory,
        },
    }
}

pub(super) fn workdir(st: &ServerState) -> PathBuf {
    match std::fs::canonicalize(&st.agent.workdir) {
        Ok(path) => path,
        Err(_) => st.agent.workdir.clone(),
    }
}
