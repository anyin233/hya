use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use yaca_core::CoreError;
use yaca_proto::ModelRef;

use crate::{ApiError, ServerState, parse_session};

#[derive(Deserialize)]
pub(super) struct SummarizePayload {
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "modelID")]
    model_id: String,
    auto: Option<bool>,
}

pub(super) async fn summarize(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(payload): Json<SummarizePayload>,
) -> Result<Json<bool>, ApiError> {
    let session = parse_session(&id)?;
    super::load_session(&st, session, None).await?;
    let _requested_model = ModelRef::new(format!("{}/{}", payload.provider_id, payload.model_id));
    let _auto = payload.auto.unwrap_or(false);
    st.engine
        .summarize_session(session)
        .await
        .map_err(summarize_error)?;
    Ok(Json(true))
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
