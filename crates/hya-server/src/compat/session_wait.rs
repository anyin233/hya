use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use hya_core::CoreError;

use crate::{ApiError, ServerState, parse_session};

pub(super) async fn compact(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match st.engine.summarize_session(session).await {
        Ok(_) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(error) => compact_error(&id, error),
    }
}

pub(super) async fn wait(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if st.engine.replay(session).await?.is_empty() {
        return Ok(super::errors::session_not_found(&id));
    }
    while st.runs.is_busy(session) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn compact_error(id: &str, error: CoreError) -> Result<Response, ApiError> {
    match error {
        CoreError::Invalid(message) if message == "summarizer not configured" => {
            Ok(super::errors::service_unavailable("compact"))
        }
        CoreError::Invalid(message) if message == "session not found" => {
            Ok(super::errors::session_not_found(id))
        }
        other => Err(ApiError::from(other)),
    }
}
