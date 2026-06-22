use axum::Json;
use axum::extract::{Query, State};
use serde::Deserialize;

use crate::{ApiError, ServerState};

#[derive(Deserialize)]
pub(super) struct ListQuery {
    roots: Option<bool>,
    search: Option<String>,
    limit: Option<usize>,
    start: Option<i64>,
}

pub(super) async fn list_sessions(
    State(st): State<ServerState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<super::projection::OpenCodeSessionInfo>>, ApiError> {
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
        let info = super::load_session(&st, session.session, Some(session.started_millis))
            .await?
            .info;
        if query.roots == Some(true) && info.parent_id().is_some() {
            continue;
        }
        if query
            .search
            .as_ref()
            .is_some_and(|search| !info.title().contains(search))
        {
            continue;
        }
        if out.len() >= limit {
            break;
        }
        out.push(info);
    }
    Ok(Json(out))
}
