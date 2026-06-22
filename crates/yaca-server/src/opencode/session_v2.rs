use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use yaca_core::CreateSession;
use yaca_proto::AgentName;

use super::model_ref::OpenCodeModelRefRequest;
use super::projection::OpenCodeSessionInfo;
use crate::{ApiError, ServerState, parse_session};

const DEFAULT_LIMIT: usize = 50;

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<usize>,
    order: Option<String>,
    search: Option<String>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct CreateV2Request {
    id: Option<String>,
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

#[derive(Serialize)]
struct SessionsResponse {
    data: Vec<OpenCodeSessionInfo>,
    cursor: SessionCursors,
}

#[derive(Serialize)]
struct SessionCursors {
    previous: Option<String>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct SessionCursor {
    id: String,
    direction: CursorDirection,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum CursorDirection {
    Previous,
    Next,
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
) -> Result<Json<SessionsResponse>, ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 {
        return Err(ApiError::bad_request("limit must be positive"));
    }
    let mut sessions = load_sessions(&st).await?;
    if query.order.as_deref() == Some("asc") {
        sessions.reverse();
    }
    if let Some(search) = query.search {
        sessions.retain(|session| session.title().contains(&search));
    }
    let start = match query.cursor {
        Some(cursor) => cursor_start(&sessions, &cursor, limit)?,
        None => 0,
    };
    let page: Vec<_> = sessions.into_iter().skip(start).take(limit).collect();
    let cursor = SessionCursors {
        previous: page
            .first()
            .map(|session| encode_cursor(session.id(), CursorDirection::Previous))
            .transpose()?,
        next: page
            .last()
            .map(|session| encode_cursor(session.id(), CursorDirection::Next))
            .transpose()?,
    };
    Ok(Json(SessionsResponse { data: page, cursor }))
}

async fn create(
    State(st): State<ServerState>,
    Json(req): Json<CreateV2Request>,
) -> Result<Json<DataResponse<OpenCodeSessionInfo>>, ApiError> {
    let requested = req.id.as_deref().map(parse_session).transpose()?;
    let session = st
        .engine
        .create_with_id(
            requested,
            CreateSession {
                parent: None,
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
) -> Result<Json<DataResponse<OpenCodeSessionInfo>>, ApiError> {
    let session = parse_session(&id)?;
    let data = super::load_session(&st, session, None).await?.info;
    Ok(Json(DataResponse { data }))
}

async fn update(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<super::session_update::UpdateSessionPayload>,
) -> Result<Json<DataResponse<OpenCodeSessionInfo>>, ApiError> {
    let session = parse_session(&id)?;
    let data = super::session_update::apply(&st, session, req).await?;
    Ok(Json(DataResponse { data }))
}

async fn remove(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<DataResponse<bool>>, ApiError> {
    let session = parse_session(&id)?;
    super::load_session(&st, session, None).await?;
    st.runs.cancel(session);
    let data = st.engine.delete_session(session).await?;
    Ok(Json(DataResponse { data }))
}

async fn init(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<super::session_legacy::InitSessionPayload>,
) -> Result<Json<DataResponse<bool>>, ApiError> {
    let session = parse_session(&id)?;
    let data = super::session_legacy::run_session_init(&st, session, req).await?;
    Ok(Json(DataResponse { data }))
}

async fn compact(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    unavailable_operation(&st, &id, "compact").await
}

async fn wait(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    unavailable_operation(&st, &id, "wait").await
}

async fn unavailable_operation(
    st: &ServerState,
    id: &str,
    operation: &str,
) -> Result<StatusCode, ApiError> {
    let session = parse_session(id)?;
    super::load_session(st, session, None).await?;
    Err(ApiError::service_unavailable(format!(
        "Session {operation} is not available yet"
    )))
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

fn cursor_start(
    sessions: &[OpenCodeSessionInfo],
    cursor: &str,
    limit: usize,
) -> Result<usize, ApiError> {
    let cursor = decode_cursor(cursor)?;
    let Some(position) = sessions
        .iter()
        .position(|session| session.id() == cursor.id.as_str())
    else {
        return Ok(sessions.len());
    };
    Ok(match cursor.direction {
        CursorDirection::Previous => position.saturating_sub(limit),
        CursorDirection::Next => position.saturating_add(1),
    })
}

fn encode_cursor(id: &str, direction: CursorDirection) -> Result<String, ApiError> {
    let raw = serde_json::json!({ "id": id, "direction": direction });
    let bytes = serde_json::to_vec(&raw).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(cursor: &str) -> Result<SessionCursor, ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| ApiError::bad_request("invalid cursor"))?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::bad_request("invalid cursor"))
}
