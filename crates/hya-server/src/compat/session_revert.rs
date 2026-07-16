use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Component, Path as FsPath, PathBuf};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use hya_proto::{Envelope, Event, Projection, SessionId};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{ApiError, ServerState, parse_session};

use super::projection::{CompatSessionSnapshot, REVERT_METADATA_KEY};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/session/:id/revert", axum::routing::post(revert))
        .route("/session/:id/unrevert", axum::routing::post(unrevert))
}

#[derive(Deserialize)]
struct RevertPayload {
    #[serde(rename = "messageID")]
    message_id: Option<String>,
    #[serde(rename = "partID")]
    part_id: Option<String>,
}

async fn revert(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(payload): Json<RevertPayload>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if st.runs.is_busy(session) {
        return Ok(super::errors::session_busy(session));
    }
    let snapshot = match load_session(&st, session).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    let Some(target) = revert_target(&snapshot, &payload) else {
        return Ok(session_response(snapshot, None));
    };
    let diffs = super::session_diff::diffs_for_target(&st, session, Some(target)).await?;
    restore_snapshots(&st, session, target, SnapshotDirection::Before).await?;
    let mut metadata = metadata_map(snapshot.info.metadata());
    metadata.insert(REVERT_METADATA_KEY.to_string(), revert_metadata(target));
    st.engine
        .set_metadata(session, Value::Object(metadata))
        .await?;
    let snapshot = match load_session(&st, session).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    Ok(session_response(snapshot, Some(&diffs)))
}

async fn unrevert(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if st.runs.is_busy(session) {
        return Ok(super::errors::session_busy(session));
    }
    let snapshot = match load_session(&st, session).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    if !snapshot.info.revert() {
        return Ok(session_response(snapshot, None));
    }
    let target = current_revert_target(&st, session)
        .await?
        .ok_or_else(|| ApiError::internal("session revert metadata missing target"))?;
    restore_snapshots(&st, session, target, SnapshotDirection::After).await?;
    st.engine
        .set_metadata(
            session,
            Value::Object(metadata_map(snapshot.info.metadata())),
        )
        .await?;
    let snapshot = match load_session(&st, session).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    Ok(session_response(snapshot, None))
}

async fn load_session(
    st: &ServerState,
    session: SessionId,
) -> Result<Result<CompatSessionSnapshot, Response>, ApiError> {
    match super::load_session(st, session, None).await {
        Ok(snapshot) => Ok(Ok(snapshot)),
        Err(error) if error.status == StatusCode::NOT_FOUND => Ok(Err(not_found_response(session))),
        Err(error) => Err(error),
    }
}

fn not_found_response(session: hya_proto::SessionId) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "name": "NotFoundError",
            "data": { "message": format!("Session not found: {session}") },
        })),
    )
        .into_response()
}

fn revert_target(
    snapshot: &CompatSessionSnapshot,
    payload: &RevertPayload,
) -> Option<super::session_diff::DiffTarget> {
    let message_id = payload.message_id.as_deref()?;
    let message = snapshot
        .messages
        .iter()
        .find(|message| message.id() == message_id)?;
    let message_id = message_id.parse().ok()?;
    let part = match payload.part_id.as_deref() {
        Some(part) if message.has_part(part) => Some(part.parse().ok()?),
        Some(_) => return None,
        None => None,
    };
    Some(super::session_diff::DiffTarget {
        message: message_id,
        part,
    })
}

fn revert_metadata(target: super::session_diff::DiffTarget) -> Value {
    let mut value = Map::from_iter([("messageID".to_string(), json!(target.message.to_string()))]);
    if let Some(part) = target.part {
        value.insert("partID".to_string(), json!(part.to_string()));
    }
    Value::Object(value)
}

fn session_response(
    snapshot: CompatSessionSnapshot,
    diffs: Option<&[super::session_diff::SessionFileDiff]>,
) -> Response {
    let mut body = serde_json::to_value(snapshot.info).unwrap_or(Value::Null);
    if let Some(diffs) = diffs
        && let Some(object) = body.as_object_mut()
    {
        object.insert("summary".to_string(), summary_value(diffs));
        if let Some(revert) = object.get_mut("revert").and_then(Value::as_object_mut) {
            let patch = combined_patch(diffs);
            if !patch.is_empty() {
                revert.insert("diff".to_string(), json!(patch));
            }
        }
    }
    Json(body).into_response()
}

fn summary_value(diffs: &[super::session_diff::SessionFileDiff]) -> Value {
    json!({
        "additions": diffs.iter().map(super::session_diff::SessionFileDiff::additions).sum::<usize>(),
        "deletions": diffs.iter().map(super::session_diff::SessionFileDiff::deletions).sum::<usize>(),
        "files": diffs.len(),
    })
}

fn combined_patch(diffs: &[super::session_diff::SessionFileDiff]) -> String {
    let mut patch = String::new();
    for diff in diffs {
        if diff.patch().is_empty() {
            continue;
        }
        if !patch.is_empty() && !patch.ends_with('\n') {
            patch.push('\n');
        }
        patch.push_str(diff.patch());
    }
    patch
}

fn metadata_map(metadata: Option<&Value>) -> Map<String, Value> {
    match metadata {
        Some(Value::Object(object)) => object.clone(),
        _ => Map::new(),
    }
}

async fn current_revert_target(
    st: &ServerState,
    session: SessionId,
) -> Result<Option<super::session_diff::DiffTarget>, ApiError> {
    let envs = st.engine.replay(session).await?;
    let projection = Projection::from_events(&envs);
    Ok(revert_target_from_metadata(
        projection.session.metadata.as_ref(),
    ))
}

#[derive(Clone, Copy)]
enum SnapshotDirection {
    Before,
    After,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileSnapshot {
    relative: PathBuf,
    before: String,
    after: String,
}

async fn restore_snapshots(
    st: &ServerState,
    session: SessionId,
    target: super::session_diff::DiffTarget,
    direction: SnapshotDirection,
) -> Result<(), ApiError> {
    let envs = st.engine.replay(session).await?;
    let projection = Projection::from_events(&envs);
    let Some(workdir) = projection.session.workdir.as_deref() else {
        return Ok(());
    };
    let workdir = FsPath::new(workdir);
    let canonical_workdir = tokio::fs::canonicalize(workdir)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let snapshots = collect_snapshots(&envs, workdir, target)?;
    for snapshot in snapshots.into_values() {
        let content = match direction {
            SnapshotDirection::Before => snapshot.before,
            SnapshotDirection::After => snapshot.after,
        };
        let path = checked_restore_path(workdir, &canonical_workdir, &snapshot.relative).await?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| ApiError::internal(error.to_string()))?;
            ensure_existing_path_within(&canonical_workdir, parent).await?;
        }
        tokio::fs::write(path, content)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
    }
    Ok(())
}

fn collect_snapshots(
    envs: &[Envelope],
    workdir: &FsPath,
    target: super::session_diff::DiffTarget,
) -> Result<BTreeMap<String, FileSnapshot>, ApiError> {
    let mut snapshots = BTreeMap::new();
    for env in envs {
        let Event::ToolResult {
            message,
            part,
            output,
            ..
        } = &env.event
        else {
            continue;
        };
        if !matches_target(*message, *part, target) {
            continue;
        }
        if let Some(snapshot) = output_snapshot(output, workdir)? {
            merge_snapshot(&mut snapshots, snapshot);
        }
    }
    Ok(snapshots)
}

fn merge_snapshot(snapshots: &mut BTreeMap<String, FileSnapshot>, snapshot: FileSnapshot) {
    let key = display_relative(&snapshot.relative);
    match snapshots.get_mut(&key) {
        Some(existing) => existing.after = snapshot.after,
        None => {
            snapshots.insert(key, snapshot);
        }
    }
}

fn matches_target(
    message: hya_proto::MessageId,
    part: hya_proto::PartId,
    target: super::session_diff::DiffTarget,
) -> bool {
    if message < target.message {
        return false;
    }
    if message == target.message && target.part.is_some_and(|target_part| part != target_part) {
        return false;
    }
    true
}

fn output_snapshot(output: &Value, workdir: &FsPath) -> Result<Option<FileSnapshot>, ApiError> {
    let Some(object) = output
        .pointer("/metadata/filediff")
        .and_then(Value::as_object)
    else {
        return Ok(None);
    };
    let Some(file) = snapshot_file_name(object) else {
        return Ok(None);
    };
    let Some(before) = object.get("beforeContent").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(after) = object.get("afterContent").and_then(Value::as_str) else {
        return Ok(None);
    };
    let relative = safe_relative_path(workdir, file)?;
    Ok(Some(FileSnapshot {
        relative,
        before: before.to_string(),
        after: after.to_string(),
    }))
}

fn snapshot_file_name(object: &Map<String, Value>) -> Option<&str> {
    ["relativePath", "file", "filePath", "filepath", "path"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
}

fn safe_relative_path(workdir: &FsPath, raw: &str) -> Result<PathBuf, ApiError> {
    let path = FsPath::new(raw);
    let relative = if path.is_absolute() {
        path.strip_prefix(workdir)
            .map_err(|_| ApiError::bad_request("cannot restore file outside session workdir"))?
    } else {
        path
    };
    clean_relative_path(relative)
        .ok_or_else(|| ApiError::bad_request("cannot restore unsafe file path"))
}

fn clean_relative_path(path: &FsPath) -> Option<PathBuf> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!relative.as_os_str().is_empty()).then_some(relative)
}

async fn checked_restore_path(
    workdir: &FsPath,
    canonical_workdir: &FsPath,
    relative: &FsPath,
) -> Result<PathBuf, ApiError> {
    let path = workdir.join(relative);
    ensure_existing_path_within(canonical_workdir, &path).await?;
    Ok(path)
}

async fn ensure_existing_path_within(
    canonical_workdir: &FsPath,
    path: &FsPath,
) -> Result<(), ApiError> {
    let mut candidate = Some(path);
    while let Some(current) = candidate {
        match tokio::fs::canonicalize(current).await {
            Ok(canonical) => {
                if canonical.starts_with(canonical_workdir) {
                    return Ok(());
                }
                return Err(ApiError::bad_request(
                    "cannot restore file through a path outside session workdir",
                ));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                candidate = current.parent();
            }
            Err(error) => return Err(ApiError::internal(error.to_string())),
        }
    }
    Err(ApiError::bad_request(
        "cannot restore file outside session workdir",
    ))
}

fn display_relative(path: &FsPath) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn revert_target_from_metadata(
    metadata: Option<&Value>,
) -> Option<super::session_diff::DiffTarget> {
    metadata
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get(REVERT_METADATA_KEY))
        .and_then(Value::as_object)
        .and_then(|revert| {
            let message = revert.get("messageID")?.as_str()?.parse().ok()?;
            let part = revert
                .get("partID")
                .and_then(Value::as_str)
                .and_then(|part| part.parse().ok());
            Some(super::session_diff::DiffTarget { message, part })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_merge_keeps_earliest_before_and_latest_after() {
        let mut snapshots = BTreeMap::new();
        merge_snapshot(
            &mut snapshots,
            FileSnapshot {
                relative: PathBuf::from("same.txt"),
                before: "old".to_string(),
                after: "middle".to_string(),
            },
        );
        merge_snapshot(
            &mut snapshots,
            FileSnapshot {
                relative: PathBuf::from("same.txt"),
                before: "middle".to_string(),
                after: "new".to_string(),
            },
        );

        assert_eq!(
            snapshots.get("same.txt"),
            Some(&FileSnapshot {
                relative: PathBuf::from("same.txt"),
                before: "old".to_string(),
                after: "new".to_string(),
            })
        );
    }

    #[test]
    fn clean_relative_path_rejects_escape_and_absolute_paths() {
        assert_eq!(
            clean_relative_path(FsPath::new("./nested/file.txt")),
            Some(PathBuf::from("nested/file.txt"))
        );
        assert_eq!(clean_relative_path(FsPath::new("../escape.txt")), None);
        assert_eq!(clean_relative_path(FsPath::new("/absolute.txt")), None);
    }
}
