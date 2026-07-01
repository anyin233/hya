use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use hya_core::AgentSpec;
use hya_proto::api::{CommandRequest, ShellRequest};
use hya_proto::{Event, MessageId, ModelRef, Projection, SessionId};
use serde::Deserialize;

use crate::{ApiError, ServerState, parse_session};

use super::projection;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/session", get(super::session_list::list_sessions))
        .route("/session/status", get(super::session_legacy_basic::status))
        .route(
            "/session/:id",
            get(super::session_legacy_basic::get_session)
                .patch(super::session_legacy_basic::update_session)
                .delete(super::session_legacy_basic::remove_session),
        )
        .route(
            "/session/:id/children",
            get(super::session_legacy_basic::children),
        )
        .route("/session/:id/todo", get(super::session_legacy_basic::todo))
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
        .route(
            "/session/:id/message",
            get(super::session_message::messages),
        )
        .route(
            "/session/:id/message/:message",
            get(super::session_message::message).delete(super::session_delete::delete_message),
        )
        .route(
            "/session/:id/message/:message/part/:part",
            delete(super::session_delete::delete_part)
                .patch(super::session_part_update::update_part),
        )
        .route(
            "/session/:id/prompt_async",
            post(super::session_prompt_async::prompt_async),
        )
        .route("/session/:id/init", post(init_session))
        .route("/session/:id/command", post(command))
        .route("/session/:id/shell", post(shell))
        .route(
            "/session/:id/abort",
            post(super::session_legacy_basic::abort),
        )
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
    let workdir = super::reference::session_workdir(&st, session).await;
    let CommandRequest {
        command,
        arguments,
        text,
    } = req;
    let text = text.unwrap_or_else(|| {
        super::command_catalog::expanded_prompt_text(&workdir, &command, &arguments)
            .unwrap_or_else(|| command_prompt_text(&command, &arguments))
    });
    let message = st
        .engine
        .admit_command_prompt(session, command, arguments, text)
        .await?;
    let agent = super::reference::session_agent_with_guidance(&st, session).await;
    let external_dirs = super::reference::external_directories_at(&st, &agent.workdir).await;
    let _finish = st
        .engine
        .run_turn_with_external_dirs(session, &agent, run.token(), &external_dirs)
        .await?;
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

pub(in crate::opencode) async fn init_agent_with_guidance(
    st: &ServerState,
    session: SessionId,
) -> AgentSpec {
    let Ok(projection) = st.engine.store().read_projection(session).await else {
        return super::reference::agent_with_guidance(st).await;
    };
    let workdir = projection
        .session
        .workdir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| super::location::workdir(st));
    super::reference::agent_with_guidance_at(st, &workdir).await
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
    let mut agent = init_agent_with_guidance(st, session).await;
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
    let external_dirs = super::reference::external_directories_at(st, &agent.workdir).await;
    let _finish = st
        .engine
        .run_turn_with_external_dirs(session, &agent, run.token(), &external_dirs)
        .await?;
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

pub(in crate::opencode) async fn legacy_load_session(
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
