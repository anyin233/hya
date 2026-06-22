use axum::extract::{Path, State};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{ApiError, ServerState, parse_session};

use super::projection::{OpenCodeSessionInfo, OpenCodeSessionSnapshot, REVERT_METADATA_KEY};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/session/:id/revert", axum::routing::post(revert))
        .route("/session/:id/unrevert", axum::routing::post(unrevert))
}

#[derive(Deserialize)]
struct RevertPayload {
    #[serde(rename = "messageID")]
    message_id: String,
    #[serde(rename = "partID")]
    part_id: Option<String>,
}

async fn revert(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(payload): Json<RevertPayload>,
) -> Result<Json<OpenCodeSessionInfo>, ApiError> {
    let session = parse_session(&id)?;
    let snapshot = super::load_session(&st, session, None).await?;
    if !target_exists(&snapshot, &payload) {
        return Ok(Json(snapshot.info));
    }
    let mut metadata = metadata_map(snapshot.info.metadata());
    metadata.insert(
        REVERT_METADATA_KEY.to_string(),
        json!({
            "messageID": payload.message_id,
            "partID": payload.part_id,
        }),
    );
    st.engine
        .set_metadata(session, Value::Object(metadata))
        .await?;
    Ok(Json(super::load_session(&st, session, None).await?.info))
}

async fn unrevert(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<OpenCodeSessionInfo>, ApiError> {
    let session = parse_session(&id)?;
    let snapshot = super::load_session(&st, session, None).await?;
    if !snapshot.info.revert() {
        return Ok(Json(snapshot.info));
    }
    st.engine
        .set_metadata(
            session,
            Value::Object(metadata_map(snapshot.info.metadata())),
        )
        .await?;
    Ok(Json(super::load_session(&st, session, None).await?.info))
}

fn target_exists(snapshot: &OpenCodeSessionSnapshot, payload: &RevertPayload) -> bool {
    if let Some(part) = &payload.part_id {
        return snapshot
            .messages
            .iter()
            .any(|message| message.has_part(part));
    }
    snapshot
        .messages
        .iter()
        .any(|message| message.id() == payload.message_id)
}

fn metadata_map(metadata: Option<&Value>) -> Map<String, Value> {
    match metadata {
        Some(Value::Object(object)) => object.clone(),
        _ => Map::new(),
    }
}
