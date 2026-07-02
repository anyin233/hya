use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use hya_proto::{MessageId, PartId, SessionId};

use crate::{ApiError, ServerState, parse_session};

pub(super) async fn delete_message(
    State(st): State<ServerState>,
    Path((id, message)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let message = parse_message(&message)?;
    if let Err(response) = ensure_session(&st, session).await? {
        return Ok(response);
    }
    if st.runs.is_busy(session) {
        return Ok(super::errors::session_busy(session));
    }
    st.engine.delete_message(session, message).await?;
    Ok(Json(true).into_response())
}

pub(super) async fn delete_part(
    State(st): State<ServerState>,
    Path((id, message, part)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let message = parse_message(&message)?;
    let part = parse_part(&part)?;
    if let Err(response) = ensure_session(&st, session).await? {
        return Ok(response);
    }
    st.engine.delete_part(session, message, part).await?;
    Ok(Json(true).into_response())
}

fn parse_message(id: &str) -> Result<MessageId, ApiError> {
    id.parse()
        .map_err(|_| ApiError::bad_request("invalid message id"))
}

fn parse_part(id: &str) -> Result<PartId, ApiError> {
    id.parse()
        .map_err(|_| ApiError::bad_request("invalid part id"))
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
