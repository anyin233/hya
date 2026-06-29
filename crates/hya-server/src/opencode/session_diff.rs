use std::collections::BTreeMap;
use std::path::Path as FsPath;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use hya_proto::{Envelope, Event, MessageId, PartId, Projection, SessionId};
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::{ApiError, ServerState, parse_session};

use super::projection::REVERT_METADATA_KEY;

#[derive(Deserialize)]
pub(super) struct DiffQuery {
    #[serde(rename = "messageID")]
    message_id: Option<String>,
    #[serde(rename = "partID")]
    part_id: Option<String>,
}

pub(super) async fn diff(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Query(query): Query<DiffQuery>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match super::load_session(&st, session, None).await {
        Ok(_) => {}
        Err(error) if error.status == axum::http::StatusCode::NOT_FOUND => {
            return Ok(super::errors::legacy_session_not_found(session));
        }
        Err(error) => return Err(error),
    }
    let envs = st.engine.replay(session).await?;
    let projection = Projection::from_events(&envs);
    let target = match query.message_id.as_deref() {
        Some(message_id) => Some(DiffTarget {
            message: match parse_message(message_id) {
                Some(message) => message,
                None => return Ok(super::errors::legacy_bad_request("invalid message id")),
            },
            part: match query.part_id.as_deref() {
                Some(part_id) => match parse_part(part_id) {
                    Some(part) => Some(part),
                    None => return Ok(super::errors::legacy_bad_request("invalid part id")),
                },
                None => None,
            },
        }),
        None => revert_target(&projection),
    };

    Ok(Json(collect_file_diffs(&envs, &projection, target)).into_response())
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DiffTarget {
    pub(super) message: MessageId,
    pub(super) part: Option<PartId>,
}

pub(super) async fn diffs_for_target(
    st: &ServerState,
    session: SessionId,
    target: Option<DiffTarget>,
) -> Result<Vec<SessionFileDiff>, ApiError> {
    let envs = st.engine.replay(session).await?;
    let projection = Projection::from_events(&envs);
    Ok(collect_file_diffs(&envs, &projection, target))
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct SessionFileDiff {
    file: String,
    patch: String,
    additions: usize,
    deletions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'static str>,
}

impl SessionFileDiff {
    pub(super) fn additions(&self) -> usize {
        self.additions
    }

    pub(super) fn deletions(&self) -> usize {
        self.deletions
    }

    pub(super) fn patch(&self) -> &str {
        &self.patch
    }
}

fn collect_file_diffs(
    envs: &[Envelope],
    projection: &Projection,
    target: Option<DiffTarget>,
) -> Vec<SessionFileDiff> {
    let Some(workdir) = projection.session.workdir.as_deref() else {
        return Vec::new();
    };
    let workdir = FsPath::new(workdir);
    let mut files = BTreeMap::<String, SessionFileDiff>::new();
    for env in envs {
        let Event::ToolResult {
            message: event_message,
            part: event_part,
            output,
            ..
        } = &env.event
        else {
            continue;
        };
        if let Some(target) = target {
            if *event_message < target.message {
                continue;
            }
            if *event_message == target.message
                && target.part.is_some_and(|part| *event_part != part)
            {
                continue;
            }
        }
        for diff in output_file_diffs(output, workdir) {
            merge_diff(&mut files, diff);
        }
    }
    files.into_values().collect()
}

fn output_file_diffs(output: &Value, workdir: &FsPath) -> Vec<SessionFileDiff> {
    let mut out = Vec::new();
    if let Some(filediff) = output.pointer("/metadata/filediff")
        && let Some(diff) = value_file_diff(filediff, workdir, Some("modified"))
    {
        out.push(diff);
    }
    if let Some(files) = output.pointer("/metadata/files").and_then(Value::as_array) {
        out.extend(files.iter().filter_map(|file| {
            value_file_diff(file, workdir, status_from_field(file.get("type")))
        }));
    }
    out
}

fn value_file_diff(
    value: &Value,
    workdir: &FsPath,
    default_status: Option<&'static str>,
) -> Option<SessionFileDiff> {
    let object = value.as_object()?;
    let file = file_name(object)?;
    let patch = object
        .get("patch")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(SessionFileDiff {
        file: normalize_file(workdir, file),
        patch,
        additions: usize_field(object.get("additions")),
        deletions: usize_field(object.get("deletions")),
        status: status_from_field(object.get("status")).or(default_status),
    })
}

fn file_name(object: &Map<String, Value>) -> Option<&str> {
    ["relativePath", "file", "filePath", "filepath", "path"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
}

fn normalize_file(workdir: &FsPath, raw: &str) -> String {
    let path = FsPath::new(raw);
    if path.is_absolute()
        && let Ok(relative) = path.strip_prefix(workdir)
    {
        return display_path(relative);
    }
    display_path(path)
}

fn display_path(path: &FsPath) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn usize_field(value: Option<&Value>) -> usize {
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
}

fn status_from_field(value: Option<&Value>) -> Option<&'static str> {
    match value.and_then(Value::as_str) {
        Some("add" | "added") => Some("added"),
        Some("delete" | "deleted") => Some("deleted"),
        Some("update" | "move" | "modify" | "modified") => Some("modified"),
        _ => None,
    }
}

fn merge_diff(files: &mut BTreeMap<String, SessionFileDiff>, diff: SessionFileDiff) {
    match files.get_mut(&diff.file) {
        Some(existing) => {
            existing.additions = existing.additions.saturating_add(diff.additions);
            existing.deletions = existing.deletions.saturating_add(diff.deletions);
            if !diff.patch.is_empty() {
                if !existing.patch.is_empty() && !existing.patch.ends_with('\n') {
                    existing.patch.push('\n');
                }
                existing.patch.push_str(&diff.patch);
            }
            if diff.status.is_some() {
                existing.status = diff.status;
            }
        }
        None => {
            files.insert(diff.file.clone(), diff);
        }
    }
}

fn revert_target(projection: &Projection) -> Option<DiffTarget> {
    projection
        .session
        .metadata
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get(REVERT_METADATA_KEY))
        .and_then(Value::as_object)
        .and_then(|revert| {
            let message = revert.get("messageID")?.as_str()?.parse().ok()?;
            let part = revert
                .get("partID")
                .and_then(Value::as_str)
                .and_then(|part| part.parse().ok());
            Some(DiffTarget { message, part })
        })
}

fn parse_message(id: &str) -> Option<MessageId> {
    id.parse().ok()
}

fn parse_part(id: &str) -> Option<PartId> {
    id.parse().ok()
}
