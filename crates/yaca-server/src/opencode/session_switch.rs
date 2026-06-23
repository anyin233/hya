use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use yaca_proto::AgentName;

use super::model_ref::OpenCodeModelRefRequest;
use crate::{ApiError, ServerState, parse_session};

#[derive(Deserialize)]
struct AgentSwitchRequest {
    agent: String,
}

#[derive(Deserialize)]
struct ModelSwitchRequest {
    model: OpenCodeModelRefRequest,
}

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/session/:id/agent", post(switch_agent))
        .route("/api/session/:id/model", post(switch_model))
}

async fn switch_agent(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<AgentSwitchRequest>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = super::session_v2::load_existing_session(&st, session, &id).await? {
        return Ok(response);
    }
    st.engine
        .switch_agent(session, AgentName::new(req.agent))
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn switch_model(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<ModelSwitchRequest>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = super::session_v2::load_existing_session(&st, session, &id).await? {
        return Ok(response);
    }
    st.engine
        .switch_model(session, req.model.into_model_ref())
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
