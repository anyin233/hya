use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;
use yaca_proto::{Projection, SessionId};

use crate::{ApiError, AppState, parse_session};

mod projection;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/session", get(list_sessions))
        .route("/session/:id", get(get_session))
        .route("/session/:id/message", get(messages))
}

async fn list_sessions(
    State(st): State<AppState>,
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
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<projection::OpenCodeSessionInfo>, ApiError> {
    let session = parse_session(&id)?;
    Ok(Json(load_session(&st, session, None).await?.info))
}

async fn messages(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<projection::OpenCodeMessage>>, ApiError> {
    let session = parse_session(&id)?;
    Ok(Json(load_session(&st, session, None).await?.messages))
}

async fn load_session(
    st: &AppState,
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
