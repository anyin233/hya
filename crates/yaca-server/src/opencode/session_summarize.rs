use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
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
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match super::load_session(&st, session, None).await {
        Ok(_) => {}
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            return Ok(super::errors::legacy_session_not_found(session));
        }
        Err(error) => return Err(error),
    }
    let _requested_model = ModelRef::new(format!("{}/{}", payload.provider_id, payload.model_id));
    let _auto = payload.auto.unwrap_or(false);
    st.engine
        .summarize_session(session)
        .await
        .map_err(summarize_error)?;
    Ok(Json(true).into_response())
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
