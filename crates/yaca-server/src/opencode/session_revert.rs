use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{ApiError, ServerState, parse_session};

use super::projection::{OpenCodeSessionSnapshot, REVERT_METADATA_KEY};

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
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let snapshot = match load_session(&st, session).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    if !target_exists(&snapshot, &payload) {
        return Ok(Json(snapshot.info).into_response());
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
    let snapshot = match load_session(&st, session).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    Ok(Json(snapshot.info).into_response())
}

async fn unrevert(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let snapshot = match load_session(&st, session).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    if !snapshot.info.revert() {
        return Ok(Json(snapshot.info).into_response());
    }
    st.engine
        .set_metadata(
            session,
            Value::Object(metadata_map(snapshot.info.metadata())),
        )
        .await?;
    let snapshot = match load_session(&st, session).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    Ok(Json(snapshot.info).into_response())
}

async fn load_session(
    st: &ServerState,
    session: yaca_proto::SessionId,
) -> Result<Result<OpenCodeSessionSnapshot, Response>, ApiError> {
    match super::load_session(st, session, None).await {
        Ok(snapshot) => Ok(Ok(snapshot)),
        Err(error) if error.status == StatusCode::NOT_FOUND => Ok(Err(not_found_response(session))),
        Err(error) => Err(error),
    }
}

fn not_found_response(session: yaca_proto::SessionId) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "name": "NotFoundError",
            "data": { "message": format!("Session not found: {session}") },
        })),
    )
        .into_response()
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
