use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use yaca_proto::{MessageId, PartId};

use crate::{ApiError, ServerState, parse_session};

pub(super) async fn delete_message(
    State(st): State<ServerState>,
    Path((id, message)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let message = parse_message(&message)?;
    let message_id = message.to_string();
    let snapshot = super::load_session(&st, session, None).await?;
    if st.runs.is_busy(session) {
        return Ok(super::errors::session_busy(session));
    }
    if !snapshot
        .messages
        .iter()
        .any(|item| item.id() == message_id.as_str())
    {
        return Err(ApiError::not_found("message not found"));
    }
    st.engine.delete_message(session, message).await?;
    Ok(Json(true).into_response())
}

pub(super) async fn delete_part(
    State(st): State<ServerState>,
    Path((id, message, part)): Path<(String, String, String)>,
) -> Result<Json<bool>, ApiError> {
    let session = parse_session(&id)?;
    let message = parse_message(&message)?;
    let part = parse_part(&part)?;
    let message_id = message.to_string();
    let part_id = part.to_string();
    let snapshot = super::load_session(&st, session, None).await?;
    let Some(found) = snapshot
        .messages
        .iter()
        .find(|item| item.id() == message_id.as_str())
    else {
        return Err(ApiError::not_found("message not found"));
    };
    if !found.has_part(&part_id) {
        return Err(ApiError::not_found("part not found"));
    }
    st.engine.delete_part(session, message, part).await?;
    Ok(Json(true))
}

fn parse_message(id: &str) -> Result<MessageId, ApiError> {
    id.parse()
        .map_err(|_| ApiError::bad_request("invalid message id"))
}

fn parse_part(id: &str) -> Result<PartId, ApiError> {
    id.parse()
        .map_err(|_| ApiError::bad_request("invalid part id"))
}
