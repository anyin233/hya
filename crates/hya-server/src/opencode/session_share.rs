use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use hya_proto::SessionId;

use crate::{ApiError, ServerState, parse_session};

pub(super) async fn share(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = ensure_session(&st, session).await? {
        return Ok(response);
    }
    st.engine
        .set_share(session, format!("hya://session/{session}"))
        .await?;
    Ok(Json(super::load_session(&st, session, None).await?.info).into_response())
}

pub(super) async fn unshare(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = ensure_session(&st, session).await? {
        return Ok(response);
    }
    st.engine.clear_share(session).await?;
    Ok(Json(super::load_session(&st, session, None).await?.info).into_response())
}

async fn ensure_session(
    st: &ServerState,
    session: SessionId,
) -> Result<Result<(), Response>, ApiError> {
    match super::load_session(st, session, None).await {
        Ok(_) => Ok(Ok(())),
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            Ok(Err(super::errors::legacy_session_not_found(session)))
        }
        Err(error) => Err(error),
    }
}
