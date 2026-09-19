//! `/v1` turn domain: the unified event-driven admission surface and turn
//! control.
//!
//! `CreateTurn` admits work and returns a handle immediately; the turn runs
//! on a spawned task and its progress arrives through the event streams.
//! `WaitTurn` exists for synchronous clients only.

use std::time::Duration;

use axum::extract::{Path as AxumPath, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::ServerState;
use hya_api::v1 as pb;
use hya_api::v1::create_turn_request::Kind;
use hya_proto::SessionId;

use super::V1Error;
use super::session::parse_session;

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/sessions/:id/turns", post(create_turn))
        .route("/v1/sessions/:id/turns/:turn", get(get_turn))
        .route("/v1/sessions/:id/turns/:turn/wait", post(wait_turn))
        .route("/v1/sessions/:id/turns/:turn/cancel", post(cancel_turn))
}

async fn ensure_session(st: &ServerState, session: SessionId) -> Result<(), V1Error> {
    if st.engine.session_exists(session).await? {
        Ok(())
    } else {
        Err(V1Error::session_not_found(&session.to_string()))
    }
}

/// Terminal-state predicate for a wire turn state.
fn is_terminal(state: i32) -> bool {
    state == pb::TurnState::Finished as i32
        || state == pb::TurnState::Failed as i32
        || state == pb::TurnState::Cancelled as i32
}

/// Current [`pb::TurnInfo`] derived from the run registry and projection.
///
/// The engine admits the user message immediately (finished on admission)
/// and drives the assistant message on the spawned run; v1 therefore
/// derives turn state from the session's busy flag and the last finished
/// assistant message, not from the admitted message alone.
pub(crate) fn turn_info(
    st: &ServerState,
    session: SessionId,
    turn: &str,
    projection: &hya_proto::Projection,
) -> pb::TurnInfo {
    if st.is_busy(session) {
        return pb::TurnInfo {
            id: turn.to_owned(),
            session: session.to_string(),
            state: pb::TurnState::Running as i32,
            finish: 0,
            error_code: String::new(),
            error_message: String::new(),
        };
    }
    let last_assistant = projection
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == hya_proto::Role::Assistant && message.finish.is_some())
        .and_then(|message| message.finish);
    let admitted = projection
        .session
        .messages
        .iter()
        .find(|message| message.id.to_string() == turn)
        .and_then(|message| message.finish);
    let (state, finish) = match last_assistant.or(admitted) {
        Some(reason) => {
            let finish = super::convert::finish_reason(reason);
            let state = match reason {
                hya_proto::FinishReason::Error => pb::TurnState::Failed as i32,
                hya_proto::FinishReason::Cancelled => pb::TurnState::Cancelled as i32,
                _ => pb::TurnState::Finished as i32,
            };
            (state, finish)
        }
        None => (pb::TurnState::Admitted as i32, 0),
    };
    pb::TurnInfo {
        id: turn.to_owned(),
        session: session.to_string(),
        state,
        finish,
        error_code: String::new(),
        error_message: String::new(),
    }
}

async fn create_turn(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<pb::CreateTurnRequest>,
) -> Result<Json<pb::CreateTurnResponse>, V1Error> {
    let session = parse_session(&id)?;
    ensure_session(&st, session).await?;

    match request.kind {
        Some(Kind::Prompt(prompt)) => {
            let run = st.start_run(session).ok_or_else(V1Error::session_busy)?;
            let message = st.engine.admit_user_prompt(session, prompt.text).await?;
            let engine = st.engine.clone();
            let turn = crate::support::reference::session_agent_with_guidance(&st, session).await;
            let external_dirs =
                crate::support::reference::external_directories_at(&st, &turn.agent.workdir).await;
            let agent = turn.agent.clone();
            let guidance = turn.guidance.clone();
            tokio::spawn(async move {
                let _ = engine
                    .run_turn_with_external_dirs_and_guidance(
                        session,
                        &agent,
                        run.token(),
                        &external_dirs,
                        guidance,
                        None,
                    )
                    .await;
                drop(run);
            });
            Ok(Json(running_turn(session, &message.to_string())))
        }
        Some(Kind::Command(command)) => {
            let native_request = hya_proto::api::CommandRequest {
                command: command.command.clone(),
                arguments: command.arguments.clone(),
                text: if command.text.is_empty() {
                    None
                } else {
                    Some(command.text.clone())
                },
                model: if command.model.is_empty() {
                    None
                } else {
                    Some(command.model.clone())
                },
                variant: None,
            };
            // Slash interception (workflow and other built-ins) runs
            // synchronously like the native command route.
            if crate::workflow::intercept_slash(&st, session, &native_request)
                .await
                .map_err(V1Error::from)?
                .is_some()
            {
                return Ok(Json(pb::CreateTurnResponse {
                    turn: Some(pb::TurnInfo {
                        id: String::new(),
                        session: session.to_string(),
                        state: pb::TurnState::Finished as i32,
                        finish: pb::FinishReason::Stop as i32,
                        error_code: String::new(),
                        error_message: String::new(),
                    }),
                }));
            }
            let run = st.start_run(session).ok_or_else(V1Error::session_busy)?;
            let explicit_model = native_request.model_ref();
            if let Some(model) = &explicit_model {
                st.engine.switch_model(session, model.clone()).await?;
            }
            // Custom slash commands from the directory catalog expand
            // server-side; unknown commands keep the literal slash.
            let text = match native_request.text.clone() {
                Some(text) => text,
                None => {
                    let workdir = st
                        .engine
                        .read_projection(session)
                        .await
                        .ok()
                        .and_then(|projection| projection.session.workdir.clone())
                        .map_or_else(|| st.agent.workdir.clone(), std::path::PathBuf::from);
                    crate::support::command_catalog::expand_prompt(
                        &workdir,
                        &command.command,
                        &command.arguments,
                    )
                    .unwrap_or_else(|| {
                        if command.arguments.trim().is_empty() {
                            format!("/{}", command.command)
                        } else {
                            format!("/{} {}", command.command, command.arguments)
                        }
                    })
                }
            };
            let message = st
                .engine
                .admit_command_prompt(
                    session,
                    command.command.clone(),
                    command.arguments.clone(),
                    text,
                )
                .await?;
            let engine = st.engine.clone();
            let turn = crate::support::reference::session_agent_with_guidance(&st, session).await;
            let external_dirs =
                crate::support::reference::external_directories_at(&st, &turn.agent.workdir).await;
            let agent = turn.agent.clone();
            let guidance = turn.guidance.clone();
            tokio::spawn(async move {
                let _ = engine
                    .run_turn_with_external_dirs_and_guidance(
                        session,
                        &agent,
                        run.token(),
                        &external_dirs,
                        guidance,
                        explicit_model,
                    )
                    .await;
                drop(run);
            });
            Ok(Json(running_turn(session, &message.to_string())))
        }
        Some(Kind::Shell(shell)) => {
            let native_request = hya_proto::api::ShellRequest {
                command: shell.command.clone(),
                agent: if shell.agent.is_empty() {
                    None
                } else {
                    Some(shell.agent.clone())
                },
                model: shell.model.map(|model| hya_proto::api::ShellModelRequest {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                }),
            };
            let agent = crate::support::reference::shell_agent(&st, session, &native_request)
                .await
                .map_err(|error| {
                    V1Error::new(hya_api::error::Code::Internal, error.text().to_owned())
                })?;
            let run = st.start_run(session).ok_or_else(V1Error::session_busy)?;
            let engine = st.engine.clone();
            let command = native_request.command.clone();
            let (message, _finish) = engine
                .run_shell(session, &agent, command, run.token())
                .await?;
            drop(run);
            Ok(Json(pb::CreateTurnResponse {
                turn: Some(pb::TurnInfo {
                    id: message.to_string(),
                    session: session.to_string(),
                    state: pb::TurnState::Finished as i32,
                    finish: pb::FinishReason::Stop as i32,
                    error_code: String::new(),
                    error_message: String::new(),
                }),
            }))
        }
        None => Err(V1Error::invalid_argument("missing turn kind")),
    }
}

fn running_turn(session: SessionId, message: &str) -> pb::CreateTurnResponse {
    pb::CreateTurnResponse {
        turn: Some(pb::TurnInfo {
            id: message.to_owned(),
            session: session.to_string(),
            state: pb::TurnState::Running as i32,
            finish: 0,
            error_code: String::new(),
            error_message: String::new(),
        }),
    }
}

async fn get_turn(
    State(st): State<ServerState>,
    AxumPath((id, turn)): AxumPath<(String, String)>,
) -> Result<Json<pb::TurnInfo>, V1Error> {
    let session = parse_session(&id)?;
    ensure_session(&st, session).await?;
    let projection = st.engine.read_projection(session).await?;
    Ok(Json(turn_info(&st, session, &turn, &projection)))
}

async fn wait_turn(
    State(st): State<ServerState>,
    AxumPath((id, turn)): AxumPath<(String, String)>,
    Query(query): Query<std::collections::BTreeMap<String, String>>,
) -> Result<Json<pb::TurnInfo>, V1Error> {
    let request: pb::WaitTurnRequest =
        super::query_request(&[("session", id.as_str()), ("turn", turn.as_str())], &query)?;
    let session = parse_session(&request.session)?;
    ensure_session(&st, session).await?;
    let deadline = if request.timeout_ms == 0 {
        None
    } else {
        Some(tokio::time::Instant::now() + Duration::from_millis(request.timeout_ms.min(600_000)))
    };
    loop {
        let projection = st.engine.read_projection(session).await?;
        let info = turn_info(&st, session, &request.turn, &projection);
        if is_terminal(info.state) || !st.is_busy(session) {
            return Ok(Json(info));
        }
        if let Some(deadline) = deadline
            && tokio::time::Instant::now() >= deadline
        {
            return Ok(Json(info));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn cancel_turn(
    State(st): State<ServerState>,
    AxumPath((id, turn)): AxumPath<(String, String)>,
) -> Result<Json<pb::TurnInfo>, V1Error> {
    let session = parse_session(&id)?;
    ensure_session(&st, session).await?;
    let cancelled = st.cancel_run(session);
    let projection = st.engine.read_projection(session).await?;
    let mut info = turn_info(&st, session, &turn, &projection);
    if cancelled {
        info.state = pb::TurnState::Cancelled as i32;
        info.finish = pb::FinishReason::Cancelled as i32;
    }
    Ok(Json(info))
}
