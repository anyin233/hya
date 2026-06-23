use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use yaca_core::CreateSession;
use yaca_proto::{AgentName, SessionId};

use super::model_ref::OpenCodeModelRefRequest;
use super::projection::OpenCodeSessionInfo;
use crate::{ApiError, ServerState, parse_session};

const DEFAULT_LIMIT: usize = 50;

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<usize>,
    order: Option<String>,
    roots: Option<bool>,
    start: Option<u64>,
    search: Option<String>,
    cursor: Option<String>,
    directory: Option<String>,
    workspace: Option<String>,
}

#[derive(Default, Deserialize)]
struct CreateV2Request {
    id: Option<String>,
    #[serde(rename = "parentID")]
    parent_id: Option<String>,
    agent: Option<String>,
    model: Option<OpenCodeModelRefRequest>,
    location: Option<LocationRefRequest>,
}

#[derive(Deserialize)]
struct LocationRefRequest {
    directory: String,
}

#[derive(Serialize)]
struct DataResponse<T> {
    data: T,
}

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/session", get(list).post(create))
        .route(
            "/api/session/:id",
            get(get_one).patch(update).delete(remove),
        )
        .route("/api/session/:id/init", post(init))
        .route("/api/session/:id/compact", post(compact))
        .route("/api/session/:id/wait", post(wait))
}

async fn list(
    State(st): State<ServerState>,
    Query(query): Query<ListQuery>,
) -> Result<Response, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if query
        .workspace
        .as_deref()
        .is_some_and(|id| !id.starts_with("wrk"))
    {
        return Ok(super::errors::invalid_workspace_query());
    }
    let decoded = match query
        .cursor
        .as_deref()
        .map(super::session_v2_cursor::decode_cursor)
        .transpose()
    {
        Ok(decoded) => decoded,
        Err(()) => return Ok(super::errors::invalid_cursor("Invalid cursor")),
    };
    let order = decoded
        .as_ref()
        .and_then(|cursor| cursor.order.as_deref())
        .or(query.order.as_deref());
    let search = decoded
        .as_ref()
        .and_then(|cursor| cursor.search.as_deref())
        .or(query.search.as_deref());
    let directory = decoded
        .as_ref()
        .and_then(|cursor| cursor.directory.as_deref())
        .or(query.directory.as_deref());
    let workspace = decoded
        .as_ref()
        .and_then(|cursor| cursor.workspace.as_deref())
        .or(query.workspace.as_deref());
    let mut sessions = load_sessions(&st).await?;
    if order == Some("asc") {
        sessions.reverse();
    }
    if query.roots == Some(true) {
        sessions.retain(|session| session.parent_id().is_none());
    }
    if let Some(start) = query.start {
        sessions.retain(|session| session.updated_millis() >= start);
    }
    if let Some(search) = search {
        sessions.retain(|session| session.title().contains(search));
    }
    if let Some(directory) = directory {
        sessions.retain(|session| session.directory() == directory);
    }
    sessions.retain(|session| !session.archived());
    let start = decoded
        .as_ref()
        .map(|cursor| super::session_v2_cursor::cursor_start(&sessions, cursor, limit))
        .unwrap_or(0);
    let page: Vec<_> = sessions.into_iter().skip(start).take(limit).collect();
    let cursor = super::session_v2_cursor::response_cursor(
        &page,
        super::session_v2_cursor::CursorParams {
            order,
            search,
            directory,
            workspace,
        },
    )?;
    Ok(Json(super::session_v2_cursor::SessionsResponse { data: page, cursor }).into_response())
}

async fn create(
    State(st): State<ServerState>,
    body: Bytes,
) -> Result<Json<DataResponse<OpenCodeSessionInfo>>, ApiError> {
    let req = if body.iter().all(|byte| byte.is_ascii_whitespace()) {
        CreateV2Request::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|_| ApiError::bad_request("invalid session create payload"))?
    };
    let requested = req.id.as_deref().map(parse_session).transpose()?;
    let parent = req.parent_id.as_deref().map(parse_session).transpose()?;
    let session = st
        .engine
        .create_with_id(
            requested,
            CreateSession {
                parent,
                agent: req
                    .agent
                    .map(AgentName::new)
                    .unwrap_or_else(|| st.agent.name.clone()),
                model: req
                    .model
                    .map(OpenCodeModelRefRequest::into_model_ref)
                    .unwrap_or_else(|| st.agent.model.clone()),
                workdir: req
                    .location
                    .map(|location| location.directory)
                    .unwrap_or_else(|| st.agent.workdir.to_string_lossy().into_owned()),
            },
        )
        .await?;
    let data = super::load_session(&st, session, None).await?.info;
    Ok(Json(DataResponse { data }))
}

async fn get_one(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let snapshot = match load_existing_session(&st, session, &id).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    Ok(Json(DataResponse {
        data: snapshot.info,
    })
    .into_response())
}

async fn update(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<super::session_update::UpdateSessionPayload>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = load_existing_session(&st, session, &id).await? {
        return Ok(response);
    }
    let data = super::session_update::apply(&st, session, req).await?;
    Ok(Json(DataResponse { data }).into_response())
}

async fn remove(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = load_existing_session(&st, session, &id).await? {
        return Ok(response);
    }
    st.runs.cancel(session);
    let data = st.engine.delete_session(session).await?;
    Ok(Json(DataResponse { data }).into_response())
}

async fn init(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<super::session_legacy::InitSessionPayload>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = load_existing_session(&st, session, &id).await? {
        return Ok(response);
    }
    let data = super::session_legacy::run_session_init(&st, session, req).await?;
    Ok(Json(DataResponse { data }).into_response())
}

async fn compact(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    super::session_unavailable::unavailable_operation(&st, &id, "compact").await
}

async fn wait(State(st): State<ServerState>, Path(id): Path<String>) -> Result<Response, ApiError> {
    super::session_unavailable::unavailable_operation(&st, &id, "wait").await
}

async fn load_sessions(st: &ServerState) -> Result<Vec<OpenCodeSessionInfo>, ApiError> {
    let sessions = st
        .engine
        .store()
        .list_sessions()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut out = Vec::with_capacity(sessions.len());
    for session in sessions {
        out.push(
            super::load_session(st, session.session, Some(session.started_millis))
                .await?
                .info,
        );
    }
    Ok(out)
}

pub(in crate::opencode) async fn load_existing_session(
    st: &ServerState,
    session: SessionId,
    id: &str,
) -> Result<Result<super::projection::OpenCodeSessionSnapshot, Response>, ApiError> {
    match super::load_session(st, session, None).await {
        Ok(snapshot) => Ok(Ok(snapshot)),
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            Ok(Err(super::errors::session_not_found(id)))
        }
        Err(error) => Err(error),
    }
}
