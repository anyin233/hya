use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use std::collections::BTreeMap;
use yaca_proto::api::PromptRequest;
use yaca_proto::{Projection, SessionId};

use crate::{ApiError, ServerState, parse_session, runs};

mod catalog;
mod file;
mod instance;
mod location;
mod metadata;
mod model_ref;
mod projection;
mod session_context;
mod session_prompt;
mod session_switch;
mod session_v2;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .merge(catalog::router())
        .merge(file::router())
        .merge(instance::router())
        .merge(metadata::router())
        .merge(session_context::router())
        .merge(session_prompt::router())
        .merge(session_v2::router())
        .merge(session_switch::router())
        .route("/session", get(list_sessions))
        .route("/session/status", get(status))
        .route("/session/:id", get(get_session))
        .route("/session/:id/children", get(children))
        .route("/session/:id/message", get(messages))
        .route("/session/:id/message/:message", get(message))
        .route("/session/:id/prompt_async", post(prompt_async))
        .route("/session/:id/abort", post(abort))
}

#[derive(Deserialize)]
struct MessagesQuery {
    before: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct MessageCursor {
    id: String,
    index: usize,
}

async fn status(State(st): State<ServerState>) -> Json<BTreeMap<String, runs::RunStatus>> {
    Json(st.runs.statuses())
}

async fn list_sessions(
    State(st): State<ServerState>,
) -> Result<Json<Vec<projection::OpenCodeSessionInfo>>, ApiError> {
    let sessions = st
        .engine
        .store()
        .list_sessions()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut out = Vec::with_capacity(sessions.len());
    for session in sessions {
        out.push(
            load_session(&st, session.session, Some(session.started_millis))
                .await?
                .info,
        );
    }
    Ok(Json(out))
}

async fn children(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<projection::OpenCodeSessionInfo>>, ApiError> {
    let parent = parse_session(&id)?;
    let sessions = st
        .engine
        .store()
        .list_sessions()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut out = Vec::new();
    let parent_id = parent.to_string();
    for session in sessions {
        let snapshot = load_session(&st, session.session, Some(session.started_millis)).await?;
        if snapshot.info.parent_id() == Some(parent_id.as_str()) {
            out.push(snapshot.info);
        }
    }
    Ok(Json(out))
}

async fn get_session(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<projection::OpenCodeSessionInfo>, ApiError> {
    let session = parse_session(&id)?;
    Ok(Json(load_session(&st, session, None).await?.info))
}

async fn messages(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Query(query): Query<MessagesQuery>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let messages = load_session(&st, session, None).await?.messages;
    if query.limit.is_none() && query.before.is_some() {
        return Err(ApiError::bad_request("before requires limit"));
    }
    let Some(limit) = query.limit.filter(|limit| *limit > 0) else {
        return Ok(Json(messages).into_response());
    };
    let before = match query.before {
        Some(before) => Some(decode_cursor(&before)?),
        None => None,
    };
    let page = page_messages(session.to_string(), messages, limit, before.as_ref())?;
    Ok(page.into_response())
}

async fn message(
    State(st): State<ServerState>,
    Path((id, message)): Path<(String, String)>,
) -> Result<Json<projection::OpenCodeMessage>, ApiError> {
    let session = parse_session(&id)?;
    let message = message
        .parse::<yaca_proto::MessageId>()
        .map_err(|_| ApiError::bad_request("invalid message id"))?
        .to_string();
    let snapshot = load_session(&st, session, None).await?;
    snapshot
        .messages
        .into_iter()
        .find(|item| item.id() == message)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("message not found"))
}

async fn prompt_async(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<PromptRequest>,
) -> Result<StatusCode, ApiError> {
    let session = parse_session(&id)?;
    load_session(&st, session, None).await?;
    let run = st
        .runs
        .start(session)
        .ok_or_else(|| ApiError::conflict("session busy"))?;
    let engine = st.engine.clone();
    let agent = st.agent.clone();
    let cancel = run.token();
    std::mem::drop(tokio::spawn(async move {
        let _guard = run;
        if engine.admit_user_prompt(session, req.text).await.is_ok() {
            let _ = engine.run_turn(session, &agent, cancel).await;
        }
    }));
    Ok(StatusCode::NO_CONTENT)
}

async fn abort(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<bool>, ApiError> {
    let session = parse_session(&id)?;
    st.runs.cancel(session);
    Ok(Json(true))
}

async fn load_session(
    st: &ServerState,
    session: SessionId,
    started_hint: Option<i64>,
) -> Result<projection::OpenCodeSessionSnapshot, ApiError> {
    let envs = st.engine.replay(session).await?;
    if envs.is_empty() {
        return Err(ApiError::not_found("session not found"));
    }
    let projection = Projection::from_events(&envs);
    projection::snapshot(session, &envs, &projection, started_hint)
        .ok_or_else(|| ApiError::not_found("session not found"))
}

fn page_messages(
    session: String,
    messages: Vec<projection::OpenCodeMessage>,
    limit: usize,
    before: Option<&MessageCursor>,
) -> Result<(HeaderMap, Json<Vec<projection::OpenCodeMessage>>), ApiError> {
    let end = before.map_or(messages.len(), |cursor| cursor.index);
    if before.is_some_and(|cursor| {
        messages
            .get(cursor.index)
            .map(projection::OpenCodeMessage::id)
            != Some(cursor.id.as_str())
    }) {
        return Err(ApiError::bad_request("invalid cursor"));
    }
    let start = end.saturating_sub(limit);
    let page: Vec<_> = messages
        .into_iter()
        .enumerate()
        .filter_map(|(idx, message)| (idx >= start && idx < end).then_some(message))
        .collect();
    let mut headers = HeaderMap::new();
    if start > 0 {
        let tail = page
            .first()
            .ok_or_else(|| ApiError::bad_request("invalid cursor"))?;
        let cursor = encode_cursor(tail.id(), start)?;
        headers.insert(
            "x-next-cursor",
            HeaderValue::from_str(&cursor).map_err(|e| ApiError::internal(e.to_string()))?,
        );
        headers.insert(
            "access-control-expose-headers",
            HeaderValue::from_static("Link, X-Next-Cursor"),
        );
        let link =
            format!("</session/{session}/message?limit={limit}&before={cursor}>; rel=\"next\"");
        headers.insert(
            "link",
            HeaderValue::from_str(&link).map_err(|e| ApiError::internal(e.to_string()))?,
        );
    }
    Ok((headers, Json(page)))
}

fn encode_cursor(id: &str, index: usize) -> Result<String, ApiError> {
    let raw = serde_json::json!({ "id": id, "index": index });
    let bytes = serde_json::to_vec(&raw).map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(cursor: &str) -> Result<MessageCursor, ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| ApiError::bad_request("invalid cursor"))?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::bad_request("invalid cursor"))
}
