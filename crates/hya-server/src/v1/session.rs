//! `/v1` session domain: lifecycle, fork, compact, summarize, revert.

use std::collections::BTreeMap;

use axum::extract::{Path as AxumPath, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::ServerState;
use hya_api::v1 as pb;
use hya_core::CreateSession;
use hya_proto::SessionId;

use super::V1Error;
use super::convert::session_info;

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/sessions", get(list_sessions).post(create_session))
        .route(
            "/v1/sessions/:id",
            get(get_session)
                .patch(axum::routing::patch(update_session))
                .delete(delete_session),
        )
        .route("/v1/sessions/:id/fork", post(fork_session))
        .route("/v1/sessions/:id/compact", post(compact_session))
        .route("/v1/sessions/:id/summarize", post(summarize_session))
        .route("/v1/sessions/:id/revert", post(revert_session))
}

pub(crate) fn parse_session(id: &str) -> Result<SessionId, V1Error> {
    id.parse::<SessionId>()
        .map_err(|_| V1Error::invalid_argument(format!("invalid session id: {id}")))
}

async fn session_exists(st: &ServerState, session: SessionId) -> Result<(), V1Error> {
    if st.engine.session_exists(session).await? {
        Ok(())
    } else {
        Err(V1Error::session_not_found(&session.to_string()))
    }
}

async fn create_session(
    State(st): State<ServerState>,
    Json(request): Json<pb::CreateSessionRequest>,
) -> Result<Json<pb::CreateSessionResponse>, V1Error> {
    if request.agent.trim().is_empty() {
        return Err(V1Error::invalid_argument("agent is required"));
    }
    if request.model.trim().is_empty() {
        return Err(V1Error::invalid_argument("model is required"));
    }
    if request.workdir.trim().is_empty() {
        return Err(V1Error::invalid_argument("workdir is required"));
    }
    let agent = crate::support::bound_agent_metadata::resolve_session_agent(
        &st,
        std::path::Path::new(&request.workdir),
        Some(request.agent.as_str()),
    )
    .await
    .map_err(|error| V1Error::new(hya_api::error::Code::Internal, error.text().to_owned()))?;
    let session = st
        .engine
        .create(CreateSession {
            parent: request.parent.parse().ok(),
            agent,
            model: hya_proto::ModelRef::new(request.model.clone()),
            workdir: request.workdir.clone(),
        })
        .await?;
    if !request.title.is_empty() {
        st.engine.set_title(session, request.title.clone()).await?;
    }
    let info = projection_info(&st, session).await?;
    Ok(Json(pb::CreateSessionResponse {
        session: Some(info),
    }))
}

/// Read one session's projection summary with store timestamps.
async fn projection_info(st: &ServerState, session: SessionId) -> Result<pb::SessionInfo, V1Error> {
    let (started, updated) = st
        .engine
        .store()
        .session_info(session)
        .await?
        .map(|row| (row.started_millis, row.updated_millis))
        .unwrap_or((0, 0));
    projection_info_at(st, session, started, updated).await
}

/// `SessionInfo` from the cached projection and already-known log bounds.
async fn projection_info_at(
    st: &ServerState,
    session: SessionId,
    started: i64,
    updated: i64,
) -> Result<pb::SessionInfo, V1Error> {
    let projection = st.engine.read_projection_shared(session).await?;
    let mut info = session_info(&projection, started, updated);
    info.busy = st.is_busy(session);
    Ok(info)
}

async fn get_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::SessionInfo>, V1Error> {
    let session = parse_session(&id)?;
    session_exists(&st, session).await?;
    Ok(Json(projection_info(&st, session).await?))
}

async fn list_sessions(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::ListSessionsResponse>, V1Error> {
    let request: pb::ListSessionsRequest = super::query_request(&[], &query)?;
    let rows = st.engine.store().list_sessions().await?;
    let mut infos = Vec::with_capacity(rows.len());
    for row in rows {
        if !request.parent.is_empty()
            && let Ok(parent) = parse_session(&request.parent)
        {
            let projection = st.engine.read_projection_shared(row.session).await?;
            let is_child = projection.session.parent == Some(parent);
            if !is_child {
                continue;
            }
        }
        // The list query already carries each log's bounds; re-listing every
        // session per row made this O(sessions x events).
        infos.push(
            projection_info_at(&st, row.session, row.started_millis, row.updated_millis).await?,
        );
    }
    infos.reverse();
    let (sessions, page) = super::catalog::paginate(infos, &request.page);
    Ok(Json(pb::ListSessionsResponse {
        sessions,
        page: Some(page),
    }))
}

async fn update_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<pb::UpdateSessionRequest>,
) -> Result<Json<pb::SessionInfo>, V1Error> {
    let session = parse_session(&id)?;
    session_exists(&st, session).await?;
    if let Some(title) = request.title
        && !title.is_empty()
    {
        st.engine.set_title(session, title).await?;
    }
    if let Some(model) = request.model
        && !model.is_empty()
    {
        st.engine
            .switch_model(session, hya_proto::ModelRef::new(model))
            .await?;
    }
    if let Some(agent) = request.agent
        && !agent.is_empty()
    {
        st.engine
            .switch_agent(session, hya_proto::AgentName::new(agent))
            .await?;
    }
    Ok(Json(projection_info(&st, session).await?))
}

async fn delete_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::DeleteSessionResponse>, V1Error> {
    let session = parse_session(&id)?;
    let deleted = st.engine.store().delete_session(session).await?;
    if !deleted {
        return Err(V1Error::session_not_found(&id));
    }
    Ok(Json(pb::DeleteSessionResponse {}))
}

async fn fork_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<pb::ForkSessionRequest>,
) -> Result<Json<pb::ForkSessionResponse>, V1Error> {
    let source = parse_session(&id)?;
    let envs = st.engine.replay(source).await?;
    if envs.is_empty() {
        return Err(V1Error::session_not_found(&id));
    }
    let projection = hya_proto::Projection::from_events(&envs);
    let before = watermark_message(&projection, request.until_seq);
    let target = st
        .engine
        .create(CreateSession {
            parent: None,
            agent: projection
                .session
                .agent
                .clone()
                .unwrap_or_else(|| st.agent.name.clone()),
            model: projection
                .session
                .model
                .clone()
                .unwrap_or_else(|| st.agent.model.clone()),
            workdir: projection
                .session
                .workdir
                .clone()
                .unwrap_or_else(|| st.agent.workdir.to_string_lossy().into_owned()),
        })
        .await?;
    st.engine
        .record_session_forked(target, source, before)
        .await?;
    st.engine
        .set_title(target, format!("forked from {source}"))
        .await?;
    if let Some(metadata) = projection.session.metadata.clone() {
        st.engine.set_metadata(target, metadata).await?;
    }
    st.engine
        .copy_messages_to_session(target, &projection, before)
        .await?;
    Ok(Json(pb::ForkSessionResponse {
        session: Some(projection_info(&st, target).await?),
    }))
}

/// Resolve the last message id at or before a sequence watermark.
fn watermark_message(
    projection: &hya_proto::Projection,
    until_seq: u64,
) -> Option<hya_proto::MessageId> {
    if until_seq == 0 {
        return projection.session.messages.last().map(|m| m.id);
    }
    // Message projections do not carry their creating sequence; approximate
    // the watermark with the transcript tail (head fork semantics).
    let _ = until_seq;
    projection.session.messages.last().map(|m| m.id)
}

async fn compact_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(_request): Json<pb::CompactSessionRequest>,
) -> Result<Json<pb::CompactSessionResponse>, V1Error> {
    let session = parse_session(&id)?;
    match st.engine.summarize_session(session).await {
        Ok(_) => {
            let projection = st.engine.read_projection(session).await?;
            Ok(Json(pb::CompactSessionResponse {
                compacted_until_seq: projection.last_seq,
                strategy: "soft".to_owned(),
            }))
        }
        Err(hya_core::CoreError::Invalid(message)) if message == "summarizer not configured" => {
            Err(V1Error::unavailable(
                "compaction summarizer is not configured",
            ))
        }
        Err(hya_core::CoreError::Invalid(message)) if message == "session not found" => {
            Err(V1Error::session_not_found(&id))
        }
        Err(other) => Err(V1Error::from(other)),
    }
}

async fn summarize_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::SummarizeSessionResponse>, V1Error> {
    let session = parse_session(&id)?;
    match st.engine.summarize_session(session).await {
        Ok(summary) => Ok(Json(pb::SummarizeSessionResponse {
            summary_message: summary.to_string(),
        })),
        Err(hya_core::CoreError::Invalid(message)) if message == "session not found" => {
            Err(V1Error::session_not_found(&id))
        }
        Err(hya_core::CoreError::Invalid(message)) if message == "summarizer not configured" => {
            Err(V1Error::unavailable("summarizer is not configured"))
        }
        Err(other) => Err(V1Error::from(other)),
    }
}

async fn revert_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<pb::RevertSessionRequest>,
) -> Result<Json<pb::RevertSessionResponse>, V1Error> {
    let session = parse_session(&id)?;
    session_exists(&st, session).await?;
    if request.undo {
        // Undo = clear any revert marker from session metadata.
        let projection = st.engine.read_projection(session).await?;
        let mut metadata = projection
            .session
            .metadata
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        metadata.remove("hya.revert");
        st.engine
            .set_metadata(session, serde_json::Value::Object(metadata))
            .await?;
        return Ok(Json(pb::RevertSessionResponse {
            session: Some(projection_info(&st, session).await?),
        }));
    }
    // Sequence-targeted revert relies on the legacy diff machinery; the
    // curated v1 surface exposes undo plus the metadata projection until
    // that machinery is ported (tracked in the consolidation plan).
    Err(V1Error::unavailable(
        "sequence-targeted revert is not ported to v1 yet; use undo=true or the legacy surface",
    ))
}
