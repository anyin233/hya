use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::http::HeaderMap;
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

#[derive(Default)]
pub(super) struct LocationRef {
    directory: Option<PathBuf>,
    workspace_id: Option<String>,
}

impl LocationRef {
    pub(super) fn from_request(query: &BTreeMap<String, String>, headers: &HeaderMap) -> Self {
        let directory = query
            .get("location[directory]")
            .cloned()
            .or_else(|| header_text(headers, "x-opencode-directory").map(|value| decode(&value)))
            .map(PathBuf::from);
        let workspace_id = query
            .get("location[workspace]")
            .cloned()
            .or_else(|| header_text(headers, "x-opencode-workspace"));
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

pub(super) fn info(st: &ServerState) -> LocationInfo {
    info_at(st, &LocationRef::default())
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
    let path = location.directory.as_ref().unwrap_or(&st.agent.workdir);
    match std::fs::canonicalize(path) {
        Ok(path) => path,
        Err(_) => path.clone(),
    }
}

fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            output.push(high * 16 + low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).unwrap_or_else(|_| input.to_string())
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
