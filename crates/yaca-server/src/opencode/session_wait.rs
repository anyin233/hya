use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::{ApiError, ServerState, parse_session};

pub(super) async fn compact(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    super::session_unavailable::unavailable_operation(&st, &id, "compact").await
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
