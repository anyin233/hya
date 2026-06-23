use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;
use yaca_proto::Projection;

use crate::{ApiError, ServerState, parse_session};

use super::session_context_messages::v2_messages;

#[derive(Serialize)]
struct ContextResponse {
    data: Vec<Value>,
}

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/api/session/:id/context", get(context))
}

async fn context(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let envs = st.engine.replay(session).await?;
    if envs.is_empty() {
        return Ok(super::errors::session_not_found(&id));
    }
    let projection = Projection::from_events(&envs);
    Ok(Json(ContextResponse {
        data: v2_messages(&envs, &projection),
    })
    .into_response())
}
