use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use hya_proto::SessionId;
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
