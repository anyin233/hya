use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use yaca_proto::{AgentName, ModelRef};

use crate::{ApiError, ServerState, parse_session};

#[derive(Deserialize)]
struct AgentSwitchRequest {
    agent: String,
}

#[derive(Deserialize)]
struct ModelSwitchRequest {
    model: ModelRefRequest,
}

#[derive(Deserialize)]
struct ModelRefRequest {
    id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "variant")]
    _variant: Option<String>,
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
) -> Result<StatusCode, ApiError> {
    let session = parse_session(&id)?;
    super::load_session(&st, session, None).await?;
    st.engine
        .switch_agent(session, AgentName::new(req.agent))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn switch_model(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<ModelSwitchRequest>,
) -> Result<StatusCode, ApiError> {
    let session = parse_session(&id)?;
    super::load_session(&st, session, None).await?;
    st.engine
        .switch_model(session, req.model.into_model_ref())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

impl ModelRefRequest {
    fn into_model_ref(self) -> ModelRef {
        ModelRef::new(format!("{}/{}", self.provider_id, self.id))
    }
}
