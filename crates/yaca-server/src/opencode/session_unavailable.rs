use axum::response::Response;

use crate::{ApiError, ServerState, parse_session};

pub(in crate::opencode) async fn unavailable_operation(
    st: &ServerState,
    id: &str,
    operation: &str,
) -> Result<Response, ApiError> {
    let session = parse_session(id)?;
    if st.engine.replay(session).await?.is_empty() {
        return Ok(super::errors::session_not_found(id));
    }
    Ok(super::errors::service_unavailable(operation))
}
