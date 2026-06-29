use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderValue;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::{ApiError, ServerState};

#[derive(Deserialize)]
pub(super) struct ListQuery {
    roots: Option<bool>,
    search: Option<String>,
    limit: Option<usize>,
    start: Option<i64>,
    cursor: Option<i64>,
    directory: Option<String>,
    archived: Option<bool>,
}

pub(super) async fn list_sessions(
    State(st): State<ServerState>,
    Query(query): Query<ListQuery>,
) -> Result<Response, ApiError> {
    let sessions = st
        .engine
        .store()
        .list_sessions()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let limit = query.limit.unwrap_or(100);
    let mut out = Vec::new();
    for session in sessions {
        if query
            .start
            .is_some_and(|start| session.updated_millis < start)
        {
            continue;
        }
        if query
            .cursor
            .is_some_and(|cursor| session.updated_millis >= cursor)
        {
            continue;
        }
        let info = super::load_session(&st, session.session, Some(session.started_millis))
            .await?
            .info;
        if info.empty_unnamed() {
            continue;
        }
        if query
            .directory
            .as_ref()
            .is_some_and(|directory| info.directory() != directory)
        {
            continue;
        }
        if query.roots == Some(true) && info.parent_id().is_some() {
            continue;
        }
        if query.archived != Some(true) && info.archived() {
            continue;
        }
        if query
            .search
            .as_ref()
            .is_some_and(|search| !info.title().contains(search))
        {
            continue;
        }
        if out.len() > limit {
            break;
        }
        out.push(info);
    }
    let has_more = out.len() > limit;
    if has_more {
        out.truncate(limit);
    }
    let next_cursor = has_more.then(|| {
        out.last()
            .map(super::projection::OpenCodeSessionInfo::updated_millis)
    });
    let mut response = Json(out).into_response();
    if let Some(Some(cursor)) = next_cursor {
        let value = HeaderValue::from_str(&cursor.to_string())
            .map_err(|e| ApiError::internal(e.to_string()))?;
        response.headers_mut().insert("x-next-cursor", value);
    }
    Ok(response)
}
