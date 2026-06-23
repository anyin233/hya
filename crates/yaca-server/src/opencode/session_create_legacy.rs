use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use yaca_core::CreateSession;
use yaca_proto::AgentName;

use super::model_ref::OpenCodeModelRefRequest;
use crate::{ApiError, ServerState, parse_session};

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/session", post(create))
}

#[derive(Default, Deserialize)]
struct CreateLegacyRequest {
    id: Option<String>,
    #[serde(rename = "parentID")]
    parent_id: Option<String>,
    parent: Option<String>,
    agent: Option<String>,
    model: Option<OpenCodeModelRefRequest>,
    location: Option<LocationRequest>,
    workdir: Option<String>,
}

#[derive(Deserialize)]
struct LocationRequest {
    directory: String,
}

async fn create(
    State(st): State<ServerState>,
    body: Bytes,
) -> Result<Json<super::projection::OpenCodeSessionInfo>, ApiError> {
    let req = parse_request(&body)?;
    let requested = req.id.as_deref().map(parse_session).transpose()?;
    let parent = req
        .parent_id
        .as_deref()
        .or(req.parent.as_deref())
        .map(parse_session)
        .transpose()?;
    let workdir = req
        .location
        .map(|location| location.directory)
        .or(req.workdir)
        .unwrap_or_else(|| st.agent.workdir.to_string_lossy().into_owned());
    let agent = req
        .agent
        .map(AgentName::new)
        .unwrap_or_else(|| default_agent(&st, &workdir));
    let session = st
        .engine
        .create_with_id(
            requested,
            CreateSession {
                parent,
                agent,
                model: req
                    .model
                    .map(OpenCodeModelRefRequest::into_model_ref)
                    .unwrap_or_else(|| st.agent.model.clone()),
                workdir,
            },
        )
        .await?;
    Ok(Json(super::load_session(&st, session, None).await?.info))
}

fn default_agent(st: &ServerState, workdir: &str) -> AgentName {
    super::agent_catalog::default_name(std::path::Path::new(workdir))
        .map(AgentName::new)
        .unwrap_or_else(|| st.agent.name.clone())
}

fn parse_request(body: &[u8]) -> Result<CreateLegacyRequest, ApiError> {
    if body.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Ok(CreateLegacyRequest::default());
    }
    serde_json::from_slice(body)
        .map_err(|_| ApiError::bad_request("invalid session create payload"))
}
