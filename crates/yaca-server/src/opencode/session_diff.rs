use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;
use yaca_proto::MessageId;

use crate::{ApiError, ServerState, parse_session};

#[derive(Deserialize)]
pub(super) struct DiffQuery {
    #[serde(rename = "messageID")]
    message_id: Option<String>,
}

pub(super) async fn diff(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Query(query): Query<DiffQuery>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match super::load_session(&st, session, None).await {
        Ok(_) => {}
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            return Ok(super::errors::legacy_session_not_found(session));
        }
        Err(error) => return Err(error),
    }
    if let Some(message_id) = query.message_id.as_deref() {
        parse_message(message_id)?;
    }
    Ok(Json(Vec::<Value>::new()).into_response())
}

fn parse_message(id: &str) -> Result<MessageId, ApiError> {
    id.parse()
        .map_err(|_| ApiError::bad_request("invalid message id"))
}
