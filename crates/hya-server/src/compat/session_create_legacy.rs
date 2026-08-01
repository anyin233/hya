use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use hya_core::CreateSession;
use serde::Deserialize;

use super::model_ref::CompatModelRefRequest;
use crate::{ApiError, ServerState, parse_session};

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/session", post(create))
}

#[derive(Default, Deserialize)]
struct CreateLegacyRequest {
    id: Option<String>,
    title: Option<String>,
    #[serde(rename = "parentID")]
    parent_id: Option<String>,
    parent: Option<String>,
    agent: Option<String>,
    model: Option<CompatModelRefRequest>,
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
) -> Result<Json<super::projection::CompatSessionInfo>, ApiError> {
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
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| st.agent.workdir.clone());
    let workdir = super::location::canonical_workdir(workdir)
        .to_string_lossy()
        .into_owned();
    // Validate against one bound catalog before any create/event side effects.
    let agent = super::bound_agent_metadata::resolve_session_agent(
        &st,
        std::path::Path::new(&workdir),
        req.agent.as_deref(),
    )
    .await?;
    let session = st
        .engine
        .create_with_id(
            requested,
            CreateSession {
                parent,
                agent,
                model: req
                    .model
                    .map(CompatModelRefRequest::into_model_ref)
                    .unwrap_or_else(|| st.agent.model.clone()),
                workdir,
            },
        )
        .await?;
    if let Some(title) = req.title {
        st.engine.set_title(session, title).await?;
    }
    Ok(Json(super::load_session(&st, session, None).await?.info))
}

fn parse_request(body: &[u8]) -> Result<CreateLegacyRequest, ApiError> {
    if body.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Ok(CreateLegacyRequest::default());
    }
    serde_json::from_slice(body)
        .map_err(|_| ApiError::bad_request("invalid session create payload"))
}
