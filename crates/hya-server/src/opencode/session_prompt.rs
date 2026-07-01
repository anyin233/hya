use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use hya_proto::api::{CommandRequest, ShellRequest};
use hya_proto::{Envelope, Event, MessageId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ApiError, ServerState, parse_session};

#[derive(Deserialize)]
struct PromptV2Request {
    id: Option<String>,
    prompt: PromptPayload,
    delivery: Option<PromptDelivery>,
    resume: Option<bool>,
}

#[derive(Clone, Deserialize, Serialize)]
struct PromptPayload {
    text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    files: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    agents: Vec<Value>,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum PromptDelivery {
    #[default]
    Steer,
    Queue,
}

#[derive(Serialize)]
struct PromptAdmittedResponse {
    data: PromptAdmitted,
}

#[derive(Serialize)]
struct MessageResponse {
    data: super::projection::OpenCodeMessage,
}

#[derive(Serialize)]
struct PromptAdmitted {
    #[serde(rename = "admittedSeq")]
    admitted_seq: u64,
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    prompt: PromptPayload,
    delivery: PromptDelivery,
    #[serde(rename = "timeCreated")]
    time_created: u64,
    #[serde(rename = "promotedSeq", skip_serializing_if = "Option::is_none")]
    promoted_seq: Option<u64>,
}

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/session/:id/prompt", post(prompt))
        .route("/api/session/:id/command", post(command))
        .route("/api/session/:id/shell", post(shell))
}

async fn prompt(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<PromptV2Request>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if st.engine.replay(session).await?.is_empty() {
        return Ok(super::errors::session_not_found(&id));
    }
    let message = match req.id.as_deref() {
        Some(id) => parse_message(id)?,
        None => MessageId::new(),
    };
    if message_exists(&st, session, message).await? {
        return Err(ApiError::conflict("prompt message id already exists"));
    }
    let run = if req.resume == Some(false) {
        None
    } else {
        Some(
            st.runs
                .start(session)
                .ok_or_else(|| ApiError::conflict("session busy"))?,
        )
    };
    let prompt = req.prompt;
    let delivery = req.delivery.unwrap_or_default();
    let admitted = st
        .engine
        .admit_user_prompt_with_id(session, message, prompt.text.clone())
        .await?;
    st.engine
        .record_user_prompt_context(
            session,
            admitted,
            prompt.files.clone(),
            prompt.agents.clone(),
        )
        .await?;
    let envelopes = st.engine.replay(session).await?;
    let (admitted_seq, time_created) = admission_info(&envelopes, admitted)?;
    set_auto_title(&st, session);
    if let Some(run) = run {
        let engine = st.engine.clone();
        let agent = super::reference::session_agent_with_guidance(&st, session).await;
        let external_dirs = super::reference::external_directories_at(&st, &agent.workdir).await;
        let cancel = run.token();
        std::mem::drop(tokio::spawn(async move {
            let _guard = run;
            let _ = engine
                .run_turn_with_external_dirs(session, &agent, cancel, &external_dirs)
                .await;
        }));
    }
    Ok(Json(PromptAdmittedResponse {
        data: PromptAdmitted {
            admitted_seq,
            id: admitted.to_string(),
            session_id: session.to_string(),
            prompt,
            delivery,
            time_created,
            promoted_seq: None,
        },
    })
    .into_response())
}

async fn command(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<CommandRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let session = parse_session(&id)?;
    super::load_session(&st, session, None).await?;
    let run = st
        .runs
        .start(session)
        .ok_or_else(|| ApiError::conflict("session busy"))?;
    let workdir = super::reference::session_workdir(&st, session).await;
    let CommandRequest {
        command,
        arguments,
        text,
    } = req;
    let text = text.unwrap_or_else(|| {
        super::command_catalog::expanded_prompt_text(&workdir, &command, &arguments)
            .unwrap_or_else(|| super::session_legacy::command_prompt_text(&command, &arguments))
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
    let data = super::session_legacy::load_message(&st, session, message).await?;
    Ok(Json(MessageResponse { data }))
}

async fn shell(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    Json(req): Json<ShellRequest>,
) -> Result<Json<MessageResponse>, ApiError> {
    let session = parse_session(&id)?;
    super::load_session(&st, session, None).await?;
    let run = st
        .runs
        .start(session)
        .ok_or_else(|| ApiError::conflict("session busy"))?;
    let (message, _finish) = st
        .engine
        .run_shell(session, &st.agent, req.command, run.token())
        .await?;
    let data = super::session_legacy::load_message(&st, session, message).await?;
    Ok(Json(MessageResponse { data }))
}

fn set_auto_title(st: &ServerState, session: SessionId) {
    let engine = st.engine.clone();
    let model = st.agent.model.clone();
    std::mem::drop(tokio::spawn(async move {
        let _ = engine.auto_title_session(session, &model).await;
    }));
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

fn admission_info(envs: &[Envelope], message: MessageId) -> Result<(u64, u64), ApiError> {
    envs.iter()
        .find_map(|env| match env.event {
            Event::MessageStarted { message: id, .. } if id == message => {
                Some((env.seq.0, millis(env.ts_millis)))
            }
            Event::SessionCreated { .. }
            | Event::SessionMoved { .. }
            | Event::SessionTitled { .. }
            | Event::SessionMetadataSet { .. }
            | Event::SessionPermissionSet { .. }
            | Event::SessionArchived { .. }
            | Event::SessionShareSet { .. }
            | Event::SessionShareCleared { .. }
            | Event::AgentSwitched { .. }
            | Event::ModelSwitched { .. }
            | Event::SessionStatus { .. }
            | Event::CommandExecuted { .. }
            | Event::UserPromptContextRecorded { .. }
            | Event::MessageStarted { .. }
            | Event::MessageFinished { .. }
            | Event::MessageDeleted { .. }
            | Event::PartDeleted { .. }
            | Event::StepStarted { .. }
            | Event::StepFinished { .. }
            | Event::TextStart { .. }
            | Event::TextDelta { .. }
            | Event::TextReplace { .. }
            | Event::TextEnd { .. }
            | Event::ReasoningStart { .. }
            | Event::ReasoningDelta { .. }
            | Event::ReasoningEnd { .. }
            | Event::ReasoningReplace { .. }
            | Event::ToolInputStart { .. }
            | Event::ToolInputDelta { .. }
            | Event::ToolCallRequested { .. }
            | Event::ToolResult { .. }
            | Event::ToolError { .. }
            | Event::ToolPartUpdated { .. }
            | Event::Error { .. } => None,
        })
        .ok_or_else(|| ApiError::internal("admitted prompt event missing"))
}

fn parse_message(id: &str) -> Result<MessageId, ApiError> {
    id.parse()
        .map_err(|_| ApiError::bad_request("invalid message id"))
}

fn millis(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}
