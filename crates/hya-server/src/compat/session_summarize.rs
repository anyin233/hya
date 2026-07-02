use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use hya_core::CoreError;
use hya_proto::{MessageId, PartProjection, Projection};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{ApiError, ServerState, parse_session};

#[derive(Deserialize)]
pub(super) struct SummarizePayload {
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "modelID")]
    model_id: String,
    auto: Option<bool>,
}

const COMPACTION_METADATA_KEY: &str = "_hyaCompatCompaction";

pub(super) async fn summarize(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(payload): Json<SummarizePayload>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match super::load_session(&st, session, None).await {
        Ok(_) => {}
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            return Ok(super::errors::legacy_session_not_found(session));
        }
        Err(error) => return Err(error),
    }
    let SummarizePayload {
        provider_id,
        model_id,
        auto,
    } = payload;
    let auto = auto.unwrap_or(false);
    let summary_message = st
        .engine
        .summarize_session(session)
        .await
        .map_err(summarize_error)?;
    let projection = st.engine.read_projection(session).await?;
    let summary_part = summary_part_id(&projection, summary_message);
    let mut metadata = metadata_map(projection.session.metadata.as_ref());
    metadata.insert(
        COMPACTION_METADATA_KEY.to_string(),
        json!({
            "type": "compaction",
            "auto": auto,
            "providerID": provider_id,
            "modelID": model_id,
            "messageID": summary_message.to_string(),
            "partID": summary_part,
        }),
    );
    st.engine
        .set_metadata(session, Value::Object(metadata))
        .await?;
    Ok(Json(true).into_response())
}

fn summary_part_id(projection: &Projection, summary_message: MessageId) -> Option<String> {
    projection
        .session
        .messages
        .iter()
        .find(|message| message.id == summary_message)
        .and_then(|message| {
            message.parts.iter().find_map(|part| match part {
                PartProjection::Text { id, .. } => Some(id.to_string()),
                PartProjection::Reasoning { .. } | PartProjection::Tool { .. } => None,
            })
        })
}

fn metadata_map(metadata: Option<&Value>) -> Map<String, Value> {
    match metadata {
        Some(Value::Object(object)) => object.clone(),
        Some(Value::Null)
        | Some(Value::Bool(_))
        | Some(Value::Number(_))
        | Some(Value::String(_))
        | Some(Value::Array(_))
        | None => Map::new(),
    }
}

fn summarize_error(error: CoreError) -> ApiError {
    match error {
        CoreError::Invalid(message) if message == "summarizer not configured" => {
            ApiError::service_unavailable(message)
        }
        CoreError::Invalid(message) if message == "session not found" => {
            ApiError::not_found(message)
        }
        other => ApiError::from(other),
    }
}
