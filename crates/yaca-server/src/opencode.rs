use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use std::collections::BTreeMap;
use yaca_proto::{Projection, SessionId};

use crate::{ApiError, ServerState, parse_session, runs};

mod file;
mod projection;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .merge(file::router())
        .route("/session", get(list_sessions))
        .route("/session/status", get(status))
        .route("/session/:id", get(get_session))
        .route("/session/:id/message", get(messages))
        .route("/session/:id/abort", post(abort))
}

async fn status(State(st): State<ServerState>) -> Json<BTreeMap<String, runs::RunStatus>> {
    Json(st.runs.statuses())
}

async fn list_sessions(
    State(st): State<ServerState>,
) -> Result<Json<Vec<projection::OpenCodeSessionInfo>>, ApiError> {
    let sessions = st
        .engine
        .store()
        .list_sessions()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut out = Vec::with_capacity(sessions.len());
    for session in sessions {
        out.push(
            load_session(&st, session.session, Some(session.started_millis))
                .await?
                .info,
        );
    }
    Ok(Json(out))
}

async fn get_session(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<projection::OpenCodeSessionInfo>, ApiError> {
    let session = parse_session(&id)?;
    Ok(Json(load_session(&st, session, None).await?.info))
}

async fn messages(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<projection::OpenCodeMessage>>, ApiError> {
    let session = parse_session(&id)?;
    Ok(Json(load_session(&st, session, None).await?.messages))
}

async fn abort(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<bool>, ApiError> {
    let session = parse_session(&id)?;
    st.runs.cancel(session);
    Ok(Json(true))
}

async fn load_session(
    st: &ServerState,
    session: SessionId,
    started_hint: Option<i64>,
) -> Result<projection::OpenCodeSessionSnapshot, ApiError> {
    let envs = st.engine.replay(session).await?;
    if envs.is_empty() {
        return Err(ApiError::not_found("session not found"));
    }
    let projection = Projection::from_events(&envs);
    projection::snapshot(session, &envs, &projection, started_hint)
        .ok_or_else(|| ApiError::not_found("session not found"))
}
