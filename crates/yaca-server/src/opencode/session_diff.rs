use axum::Json;
use axum::extract::{Path, Query, State};
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
) -> Result<Json<Vec<Value>>, ApiError> {
    let session = parse_session(&id)?;
    super::load_session(&st, session, None).await?;
    if let Some(message_id) = query.message_id.as_deref() {
        parse_message(message_id)?;
    }
    Ok(Json(Vec::new()))
}

fn parse_message(id: &str) -> Result<MessageId, ApiError> {
    id.parse()
        .map_err(|_| ApiError::bad_request("invalid message id"))
}
