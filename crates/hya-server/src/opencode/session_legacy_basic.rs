use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::{ApiError, ServerState, parse_session, runs};

pub(super) async fn status(
    State(st): State<ServerState>,
) -> Json<BTreeMap<String, runs::RunStatus>> {
    Json(st.runs.statuses())
}

pub(super) async fn children(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let parent = parse_session(&id)?;
    if let Err(response) = super::session_legacy::legacy_load_session(&st, parent, None).await? {
        return Ok(response);
    }
    let sessions = st
        .engine
        .store()
        .list_sessions()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut out = Vec::new();
    let parent_id = parent.to_string();
    for session in sessions {
        let snapshot =
            super::session_legacy::load_session(&st, session.session, Some(session.started_millis))
                .await?;
        if snapshot.info.parent_id() == Some(parent_id.as_str()) {
            out.push(snapshot.info);
        }
    }
    Ok(Json(out).into_response())
}

pub(super) async fn get_session(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match super::session_legacy::legacy_load_session(&st, session, None).await? {
        Ok(snapshot) => Ok(Json(snapshot.info).into_response()),
        Err(response) => Ok(response),
    }
}

/// Recursive subagent run tree rooted at the requested session's top ancestor.
/// Joins each session's `members[].child` links via the shared `build_run_tree`
/// assembler, so nested subagents are visible as one tree (parent → children).
pub(super) async fn tree(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = super::session_legacy::legacy_load_session(&st, session, None).await? {
        return Ok(response);
    }
    let (root, _) = st
        .engine
        .session_lineage(session)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut lookup: std::collections::HashMap<_, _> = std::collections::HashMap::new();
    let mut queue = vec![root];
    while let Some(sid) = queue.pop() {
        if lookup.contains_key(&sid) {
            continue;
        }
        let projection = st
            .engine
            .read_projection(sid)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
        for member in &projection.session.members {
            if let Some(child) = member.child
                && !lookup.contains_key(&child)
            {
                queue.push(child);
            }
        }
        lookup.insert(sid, projection.session);
    }
    let tree = hya_proto::build_run_tree(root, &lookup);
    Ok(Json(tree).into_response())
}

pub(super) async fn update_session(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(payload): Json<super::session_update::UpdateSessionPayload>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match super::session_update::apply(&st, session, payload).await {
        Ok(info) => Ok(Json(info).into_response()),
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            Ok(super::errors::legacy_session_not_found(session))
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn remove_session(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = super::session_legacy::legacy_load_session(&st, session, None).await? {
        return Ok(response);
    }
    st.runs.cancel(session);
    let deleted = st.engine.delete_session(session).await?;
    Ok(Json(deleted).into_response())
}

pub(super) async fn todo(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = super::session_legacy::legacy_load_session(&st, session, None).await? {
        return Ok(response);
    }
    Ok(Json(st.engine.todos(session).await).into_response())
}

pub(super) async fn abort(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<bool>, ApiError> {
    let session = parse_session(&id)?;
    st.runs.cancel(session);
    Ok(Json(true))
}
