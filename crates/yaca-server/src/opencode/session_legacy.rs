use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::http::header::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::json;
use yaca_proto::api::{CommandRequest, PromptRequest, ShellRequest};
use yaca_proto::{Envelope, Event, MessageId, ModelRef, Projection, SessionId, now_millis};

use crate::{ApiError, ServerState, parse_session, runs};

use super::projection;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/session", get(super::session_list::list_sessions))
        .route("/session/status", get(status))
        .route(
            "/session/:id",
            get(get_session)
                .patch(update_session)
                .delete(remove_session),
        )
        .route("/session/:id/children", get(children))
        .route("/session/:id/todo", get(todo))
        .route("/session/:id/diff", get(super::session_diff::diff))
        .route(
            "/session/:id/share",
            post(super::session_share::share).delete(super::session_share::unshare),
        )
        .route("/session/:id/fork", post(super::session_fork::fork))
        .route(
            "/session/:id/summarize",
            post(super::session_summarize::summarize),
        )
        .route("/session/:id/message", get(messages))
        .route(
            "/session/:id/message/:message",
            get(message).delete(super::session_delete::delete_message),
        )
        .route(
            "/session/:id/message/:message/part/:part",
            delete(super::session_delete::delete_part)
                .patch(super::session_part_update::update_part),
        )
        .route("/session/:id/prompt_async", post(prompt_async))
        .route("/session/:id/init", post(init_session))
        .route("/session/:id/command", post(command))
        .route("/session/:id/shell", post(shell))
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

#[derive(Deserialize)]
pub(in crate::opencode) struct InitSessionPayload {
    #[serde(rename = "messageID")]
    message_id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "modelID")]
    model_id: String,
}

async fn status(State(st): State<ServerState>) -> Json<BTreeMap<String, runs::RunStatus>> {
    Json(st.runs.statuses())
}

async fn children(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let parent = parse_session(&id)?;
    if let Err(response) = legacy_load_session(&st, parent, None).await? {
        return Ok(response);
    }
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
    Ok(Json(out).into_response())
}

async fn get_session(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match legacy_load_session(&st, session, None).await? {
        Ok(snapshot) => Ok(Json(snapshot.info).into_response()),
        Err(response) => Ok(response),
    }
}

async fn update_session(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(payload): Json<super::session_update::UpdateSessionPayload>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match super::session_update::apply(&st, session, payload).await {
        Ok(info) => Ok(Json(info).into_response()),
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            Ok(super::errors::legacy_session_not_found(session))
        }
        Err(error) => Err(error),
    }
}

async fn remove_session(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = legacy_load_session(&st, session, None).await? {
        return Ok(response);
    }
    st.runs.cancel(session);
    let deleted = st.engine.delete_session(session).await?;
    Ok(Json(deleted).into_response())
}

async fn todo(State(st): State<ServerState>, Path(id): Path<String>) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = legacy_load_session(&st, session, None).await? {
        return Ok(response);
    }
    Ok(Json(st.engine.todos(session).await).into_response())
}

async fn messages(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Query(query): Query<MessagesQuery>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let messages = match legacy_load_session(&st, session, None).await? {
        Ok(snapshot) => snapshot.messages,
        Err(response) => return Ok(response),
    };
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
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    let message = message
        .parse::<yaca_proto::MessageId>()
        .map_err(|_| ApiError::bad_request("invalid message id"))?
        .to_string();
    let snapshot = match legacy_load_session(&st, session, None).await? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    let message = snapshot
        .messages
        .into_iter()
        .find(|item| item.id() == message)
        .ok_or_else(|| ApiError::not_found("message not found"))?;
    Ok(Json(message).into_response())
}

async fn prompt_async(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<PromptRequest>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match load_session(&st, session, None).await {
        Ok(_) => {}
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            return Ok(super::errors::legacy_session_not_found(session));
        }
        Err(error) => return Err(error),
    }
    let runs = st.runs.clone();
    let engine = st.engine.clone();
    let agent = st.agent.clone();
    std::mem::drop(tokio::spawn(async move {
        let Some(run) = runs.start(session) else {
            publish_background_error(&engine, session, "session busy".to_string()).await;
            return;
        };
        let cancel = run.token();
        let guard = run;
        publish_session_status(&engine, session, "busy").await;
        let result = async {
            engine.admit_user_prompt(session, req.text).await?;
            engine.run_turn(session, &agent, cancel).await?;
            Ok::<(), yaca_core::CoreError>(())
        }
        .await;
        if let Err(error) = result {
            publish_background_error(&engine, session, error.to_string()).await;
        }
        drop(guard);
        publish_session_status(&engine, session, "idle").await;
    }));
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn publish_session_status(
    engine: &yaca_core::SessionEngine,
    session: SessionId,
    status_type: &'static str,
) {
    publish_background_event(
        engine,
        session,
        Event::SessionStatus {
            session,
            status: json!({ "type": status_type }),
        },
    )
    .await;
}

async fn publish_background_error(
    engine: &yaca_core::SessionEngine,
    session: SessionId,
    message: String,
) {
    let event = Event::Error {
        session: Some(session),
        code: "prompt_async".to_string(),
        message,
    };
    publish_background_event(engine, session, event).await;
}

async fn publish_background_event(
    engine: &yaca_core::SessionEngine,
    session: SessionId,
    event: Event,
) {
    let Ok(seq) = engine.store().append_event(session, &event).await else {
        return;
    };
    engine.bus().publish(Envelope {
        seq,
        ts_millis: now_millis(),
        event,
    });
}

async fn init_session(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<InitSessionPayload>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if st.runs.is_busy(session) {
        return Ok(super::errors::legacy_bad_request("Bad request"));
    }
    match run_session_init(&st, session, req).await {
        Ok(initialized) => Ok(Json(initialized).into_response()),
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            Ok(super::errors::legacy_session_not_found(session))
        }
        Err(error) => Err(error),
    }
}

async fn command(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<CommandRequest>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match load_session(&st, session, None).await {
        Ok(_) => {}
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            return Ok(super::errors::legacy_session_not_found(session));
        }
        Err(error) => return Err(error),
    }
    let Some(run) = st.runs.start(session) else {
        return Ok(super::errors::legacy_bad_request("Bad request"));
    };
    let text = req
        .text
        .unwrap_or_else(|| command_prompt_text(&req.command, &req.arguments));
    let message = st
        .engine
        .admit_command_prompt(session, req.command, req.arguments, text)
        .await?;
    let _finish = st.engine.run_turn(session, &st.agent, run.token()).await?;
    Ok(Json(load_message(&st, session, message).await?).into_response())
}

async fn shell(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<ShellRequest>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match load_session(&st, session, None).await {
        Ok(_) => {}
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            return Ok(super::errors::legacy_session_not_found(session));
        }
        Err(error) => return Err(error),
    }
    let Some(run) = st.runs.start(session) else {
        return Ok(super::errors::session_busy(session));
    };
    let (message, _finish) = st
        .engine
        .run_shell(session, &st.agent, req.command, run.token())
        .await?;
    Ok(Json(load_message(&st, session, message).await?).into_response())
}

async fn abort(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<bool>, ApiError> {
    let session = parse_session(&id)?;
    st.runs.cancel(session);
    Ok(Json(true))
}

pub(in crate::opencode) async fn load_message(
    st: &ServerState,
    session: SessionId,
    message: MessageId,
) -> Result<projection::OpenCodeMessage, ApiError> {
    let message_id = message.to_string();
    load_session(st, session, None)
        .await?
        .messages
        .into_iter()
        .find(|item| item.id() == message_id)
        .ok_or_else(|| ApiError::not_found("message not found"))
}

pub(in crate::opencode) async fn run_session_init(
    st: &ServerState,
    session: SessionId,
    req: InitSessionPayload,
) -> Result<bool, ApiError> {
    load_session(st, session, None).await?;
    let message = req
        .message_id
        .parse::<MessageId>()
        .map_err(|_| ApiError::bad_request("invalid message id"))?;
    if message_exists(st, session, message).await? {
        return Err(ApiError::conflict("prompt message id already exists"));
    }
    let run = st
        .runs
        .start(session)
        .ok_or_else(|| ApiError::bad_request("Bad request"))?;
    let mut agent = (*st.agent).clone();
    agent.model = ModelRef::new(format!("{}/{}", req.provider_id, req.model_id));
    st.engine
        .admit_command_prompt_with_id(
            session,
            message,
            "init".to_string(),
            String::new(),
            "/init".to_string(),
        )
        .await?;
    let _finish = st.engine.run_turn(session, &agent, run.token()).await?;
    Ok(true)
}

pub(in crate::opencode) async fn load_session(
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

async fn legacy_load_session(
    st: &ServerState,
    session: SessionId,
    started_hint: Option<i64>,
) -> Result<Result<projection::OpenCodeSessionSnapshot, Response>, ApiError> {
    match load_session(st, session, started_hint).await {
        Ok(snapshot) => Ok(Ok(snapshot)),
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            Ok(Err(super::errors::legacy_session_not_found(session)))
        }
        Err(error) => Err(error),
    }
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

async fn message_exists(
    st: &ServerState,
    session: SessionId,
    message: MessageId,
) -> Result<bool, ApiError> {
    Ok(st.engine.replay(session).await?.into_iter().any(|env| {
        matches!(
            env.event,
            Event::MessageStarted {
                message: existing,
                ..
            } if existing == message
        )
    }))
}

pub(in crate::opencode) fn command_prompt_text(command: &str, arguments: &str) -> String {
    if arguments.trim().is_empty() {
        format!("/{command}")
    } else {
        format!("/{command} {arguments}")
    }
}
