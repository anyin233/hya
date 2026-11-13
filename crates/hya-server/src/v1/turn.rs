//! `/v1` turn domain: the unified event-driven admission surface and turn
//! control.
//!
//! `CreateTurn` prepares the turn, reserves the session, and hands both to a
//! spawned task that admits the prompt and runs the turn; the call returns
//! once the prompt is admitted and progress arrives through the event streams.
//! `WaitTurn` exists for synchronous clients only.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::routing::{get, post};

use super::Json;

use crate::ServerState;
use crate::runs::RunGuard;
use hya_api::v1 as pb;
use hya_api::v1::create_turn_request::Kind;
use hya_core::attachments::PromptAttachment;
use hya_proto::{MessageId, ModelRef, SessionId};

use super::V1Error;
use super::session::parse_session;

/// Turn inputs prepared before the run is reserved: everything slow or
/// fallible a turn needs runs before anything is admitted, so a failure
/// admits nothing and a stop never waits on it.
struct PreparedTurn {
    agent: hya_core::AgentSpec,
    guidance: Option<Arc<str>>,
    external_dirs: Vec<PathBuf>,
    explicit_model: Option<ModelRef>,
}

async fn prepare_turn(
    st: &ServerState,
    session: SessionId,
    explicit_model: Option<ModelRef>,
) -> Result<PreparedTurn, V1Error> {
    let turn = crate::support::reference::session_agent_with_guidance(
        st,
        session,
        explicit_model.as_ref(),
    )
    .await?;
    let external_dirs =
        crate::support::reference::external_directories_at(st, &turn.agent.workdir).await;
    Ok(PreparedTurn {
        agent: turn.agent,
        guidance: turn.guidance,
        external_dirs,
        explicit_model,
    })
}

/// What a reserved run's turn task commits before it runs the turn.
enum Admission {
    /// A client prompt, under the id the response already names.
    Prompt {
        message: MessageId,
        text: String,
        attachments: Vec<PromptAttachment>,
    },
    /// An expanded slash command, under the id the response already names.
    Command {
        message: MessageId,
        command: String,
        arguments: String,
        text: String,
    },
    /// The session's pending restart restore, if it still applies.
    Restore,
}

/// A run reserved before the server began to stop admits nothing.
fn refuse_if_stopping(st: &ServerState) -> Result<(), V1Error> {
    if st.runs.draining() {
        return Err(V1Error::unavailable("the server is stopping"));
    }
    Ok(())
}

/// Commit the run's prompt: the client prompt or command (bringing an
/// archived session back and lifting a user stop first), or the restore
/// prompt. `None`: no restore applies any more.
async fn admit(
    st: &ServerState,
    session: SessionId,
    admission: Admission,
    turn: &PreparedTurn,
) -> Result<Option<MessageId>, V1Error> {
    refuse_if_stopping(st)?;
    ensure_session(st, session).await?;
    let message = match admission {
        Admission::Restore => return Ok(st.engine.inject_restart_prompt(session).await?),
        Admission::Prompt {
            message,
            text,
            attachments,
        } => {
            super::session::unarchive_for_turn(st, session).await?;
            st.engine.resume_after_user_stop(session);
            st.engine
                .admit_user_prompt_with_attachments(session, message, text, attachments)
                .await?
        }
        Admission::Command {
            message,
            command,
            arguments,
            text,
        } => {
            super::session::unarchive_for_turn(st, session).await?;
            st.engine.resume_after_user_stop(session);
            if let Some(model) = &turn.explicit_model {
                st.engine.switch_model(session, model.clone()).await?;
            }
            st.engine
                .admit_command_prompt_with_id(session, message, command, arguments, text)
                .await?
        }
    };
    Ok(Some(message))
}

/// Hand a reserved run to its turn task and wait until the task admitted its
/// prompt. The task owns everything from admission to the turn's closing
/// assistant pair, and holds `run` until the turn ends (clients see the
/// session busy for exactly that time; a stop waits for it), so dropping the
/// caller strands nothing. `false`: a restore found nothing to restore.
async fn spawn_turn_run(
    st: &ServerState,
    session: SessionId,
    run: RunGuard,
    turn: PreparedTurn,
    admission: Admission,
) -> Result<bool, V1Error> {
    if let Some((paused, barrier)) = &st.turn_spawn_barrier
        && *paused == session
    {
        barrier.wait().await;
        barrier.wait().await;
    }
    let (admitted, admission_result) = tokio::sync::oneshot::channel();
    let st = st.clone();
    tokio::spawn(async move {
        if run.admission_closed() {
            drop(run);
            let _ = admitted.send(Err(V1Error::unavailable(
                "the session run was cancelled before admission",
            )));
            return;
        }
        let title_after_admission = !matches!(&admission, Admission::Restore);
        let restore_lease = if title_after_admission {
            None
        } else {
            match st.engine.try_begin_turn(session) {
                Ok(lease) => Some(lease),
                Err(error) => {
                    drop(run);
                    let _ = admitted.send(Err(V1Error::from(error)));
                    return;
                }
            }
        };
        let prompt = match admit(&st, session, admission, &turn).await {
            Ok(Some(prompt)) => prompt,
            result => {
                drop(restore_lease);
                drop(run);
                #[cfg(test)]
                if let Some(hook) = &st.admission_reply_hook {
                    hook();
                }
                let _ = admitted.send(result.map(|_| false));
                return;
            }
        };
        let _ = admitted.send(Ok(true));
        let execution = async {
            if let Some(lease) = restore_lease {
                st.engine
                    .run_admitted_turn(
                        lease,
                        &turn.agent,
                        run.token(),
                        &turn.external_dirs,
                        turn.guidance,
                        turn.explicit_model,
                        prompt,
                    )
                    .await
            } else {
                st.engine
                    .run_turn_with_external_dirs_and_guidance(
                        session,
                        &turn.agent,
                        run.token(),
                        &turn.external_dirs,
                        turn.guidance,
                        turn.explicit_model,
                        Some(prompt),
                    )
                    .await
            }
        };
        let title = async {
            if title_after_admission {
                auto_title(&st, session, &turn.agent.model, run.token()).await;
            }
        };
        let (result, ()) = tokio::join!(execution, title);
        if let Err(error) = result {
            tracing::error!(%session, %error, "server turn failed");
        }
        drop(run);
    });
    admission_result
        .await
        .map_err(|_| V1Error::internal("the turn task ended before admitting its prompt"))?
}

impl crate::AppState {
    /// Restore interrupted roots through the ordinary server turn context and
    /// run registry. A failed session is logged and does not block startup.
    pub async fn resume_interrupted_turns(&self) -> Vec<SessionId> {
        let st = self.server_state();
        let rows = match st.engine.store().list_pending_resumes().await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, "cannot list pending restores");
                return Vec::new();
            }
        };
        let mut resumed = Vec::new();
        for row in rows {
            let session = row.session;
            match resume_turn(&st, session).await {
                Ok(true) => resumed.push(session),
                Ok(false) => {}
                Err(error) => {
                    tracing::error!(%session, error = %error.message, "cannot restore interrupted turn")
                }
            }
        }
        resumed
    }
}

async fn resume_turn(st: &ServerState, session: SessionId) -> Result<bool, V1Error> {
    if st.is_busy(session) {
        return Err(V1Error::session_busy());
    }
    let turn = prepare_turn(st, session, None).await?;
    let run = st.start_run(session).ok_or_else(V1Error::session_busy)?;
    spawn_turn_run(st, session, run, turn, Admission::Restore).await
}

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        // Prompt attachments ride in the body (base64 in JSON): lift the
        // default 2 MB extractor limit for this route only.
        .route(
            "/v1/sessions/:id/turns",
            post(create_turn).layer(DefaultBodyLimit::max(crate::MAX_TURN_REQUEST_BYTES)),
        )
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
        .find(|message| message.role == hya_proto::Role::Assistant && message.finish.is_some());
    let admitted = projection
        .session
        .messages
        .iter()
        .find(|message| message.id.to_string() == turn)
        .and_then(|message| message.finish);
    // A failed turn reports the error the engine recorded on the failed
    // assistant message (`Event::Error { failed_message }`).
    let (error_code, error_message) = last_assistant
        .filter(|message| message.finish == Some(hya_proto::FinishReason::Error))
        .and_then(|message| message.error.as_ref())
        .map(|error| (error.code.clone(), error.message.clone()))
        .unwrap_or_default();
    let (state, finish) = match last_assistant
        .and_then(|message| message.finish)
        .or(admitted)
    {
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
        error_code,
        error_message,
    }
}

/// Title the session in the background after a prompt was admitted, when
/// enabled. The run owns both this call and its turn, so deletion cancels
/// and joins every writer. A title failure never fails the turn.
async fn auto_title(
    st: &ServerState,
    session: SessionId,
    model: &ModelRef,
    cancel: tokio_util::sync::CancellationToken,
) {
    if !st.auto_title {
        return;
    }
    tokio::select! {
        biased;
        () = cancel.cancelled() => {},
        result = st.engine.auto_title_session(session, model) => {
            if let Err(error) = result {
                tracing::warn!(%session, "automatic session title failed: {error}");
            }
        }
    }
}

async fn create_turn(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<pb::CreateTurnRequest>,
) -> Result<Json<pb::CreateTurnResponse>, V1Error> {
    let session = parse_session(&id)?;
    ensure_session(&st, session).await?;
    st.engine.ensure_external_turn_allowed(session).await?;

    match request.kind {
        Some(Kind::Prompt(prompt)) => {
            let attachments = prompt_attachments(prompt.attachments)?;
            let turn = prepare_turn(&st, session, None).await?;
            if !attachments.is_empty() {
                ensure_image_input(&st, session, &turn.agent).await?;
            }
            let run = st.start_run(session).ok_or_else(V1Error::session_busy)?;
            let message = MessageId::new();
            let admission = Admission::Prompt {
                message,
                text: prompt.text,
                attachments,
            };
            spawn_turn_run(&st, session, run, turn, admission).await?;
            Ok(Json(running_turn(session, &message.to_string())))
        }
        Some(Kind::Command(command)) => {
            // Slash interception (workflow and other built-ins) runs
            // synchronously like the native command route.
            if crate::workflow::intercept_slash(&st, session, &command)
                .await?
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
            let explicit_model =
                (!command.model.trim().is_empty()).then(|| ModelRef::new(command.model.trim()));
            // Custom slash commands from the session's catalog (its workdir,
            // then its Project's roots) expand server-side; unknown commands
            // keep the literal slash.
            let text = if !command.text.is_empty() {
                command.text.clone()
            } else {
                let place =
                    crate::support::catalog_place::CatalogPlace::for_session(&st, session).await?;
                crate::support::command_catalog::expand_prompt(
                    &place,
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
            };
            let turn = prepare_turn(&st, session, explicit_model).await?;
            let run = st.start_run(session).ok_or_else(V1Error::session_busy)?;
            let message = MessageId::new();
            let admission = Admission::Command {
                message,
                command: command.command,
                arguments: command.arguments,
                text,
            };
            spawn_turn_run(&st, session, run, turn, admission).await?;
            Ok(Json(running_turn(session, &message.to_string())))
        }
        Some(Kind::Shell(shell)) => {
            let agent = crate::support::reference::shell_agent(&st, session, &shell)
                .await
                .map_err(|error| {
                    V1Error::new(hya_api::error::Code::Internal, error.text().to_owned())
                })?;
            let run = st.start_run(session).ok_or_else(V1Error::session_busy)?;
            // The task owns the shell turn from its note's admission to its
            // closing pair; the handler only waits for the outcome.
            let task = st.clone();
            let (message, _finish) = tokio::spawn(async move {
                refuse_if_stopping(&task)?;
                // A new turn brings an archived session back.
                super::session::unarchive_for_turn(&task, session).await?;
                let outcome = task
                    .engine
                    .run_shell(session, &agent, shell.command, run.token())
                    .await;
                drop(run);
                Ok::<_, V1Error>(outcome?)
            })
            .await
            .map_err(|error| V1Error::internal(format!("shell turn task failed: {error}")))??;
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

/// Validate a prompt's attachments ([`hya_core::attachments`]): image types
/// only, 10 MiB each, 20 MiB per turn. Any failure is `invalid_argument`.
fn prompt_attachments(
    attachments: Vec<pb::PromptAttachment>,
) -> Result<Vec<PromptAttachment>, V1Error> {
    let mut attachments = attachments
        .into_iter()
        .map(|attachment| PromptAttachment {
            name: attachment.name,
            mime: attachment.mime,
            data: attachment.data,
            path: Some(attachment.path),
        })
        .collect::<Vec<_>>();
    hya_core::attachments::validate_prompt_attachments(&mut attachments)
        .map_err(|error| V1Error::invalid_argument(error.to_string()))?;
    Ok(attachments)
}

/// Refuse images for a model whose route declares no image input
/// (`Capabilities::image_input == Some(false)`); unknown support is allowed.
async fn ensure_image_input(
    st: &ServerState,
    session: SessionId,
    agent: &hya_core::AgentSpec,
) -> Result<(), V1Error> {
    let model = st.engine.root_turn_model(session, agent).await?;
    let image_input = st
        .engine
        .provider_router()
        .capabilities(&model)
        .and_then(|capabilities| capabilities.image_input);
    if image_input == Some(false) {
        return Err(V1Error::invalid_argument(format!(
            "model `{model}` does not accept image input; switch to a model that does or send the prompt without attachments"
        )));
    }
    Ok(())
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
    // The resident substrate may release and re-acquire the admission slot
    // across one logical turn (parent continuation after a non-blocking
    // spawn): a single idle sample is not completion. Return on a terminal
    // turn state, or on idle sustained across consecutive samples.
    let mut idle_samples = 0u8;
    loop {
        let projection = st.engine.read_projection(session).await?;
        let info = turn_info(&st, session, &request.turn, &projection);
        if is_terminal(info.state) {
            return Ok(Json(info));
        }
        idle_samples = if st.is_busy(session) {
            0
        } else {
            idle_samples.saturating_add(1)
        };
        if idle_samples >= 3 {
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


#[cfg(test)]
mod admission_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use hya_core::{AgentCatalog, AgentSpec, CreateSession, EventBus, RuntimeRegistry, SessionEngine};
    use hya_proto::{AgentName, ModelRef};
    use hya_provider::ProviderRouter;
    use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

    #[tokio::test]
    async fn o6_empty_and_failed_restores_release_both_reservations_before_reply() {
        let catalog = hya_bundle::BundleCatalog::from_prepared(&[]).unwrap();
        let runtime = RuntimeRegistry::from_snapshot(
            ToolRegistry::from_tool_families(&[]).snapshot(),
            Arc::new(AgentCatalog::new(Arc::new(catalog)).unwrap()),
        );
        let (permission, _) = PermissionPlane::new(PermissionRules::default());
        let engine = Arc::new(SessionEngine::new(
            hya_store::SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new()), Arc::new(runtime), permission, EventBus::default(),
        ));
        let agent = Arc::new(AgentSpec {
            name: AgentName::new("hya-main"), model: ModelRef::new("hya/offline"),
            workdir: std::env::temp_dir(), system_prompt: String::new(), reasoning: None,
        });
        engine.store().claim_runtime_owner(engine.runtime_owner()).unwrap();
        let app = crate::AppState::new(engine.clone(), agent.clone());
        let mut st = ServerState::new(app.clone());
        let session = engine.create(CreateSession {
            parent: None, agent: agent.name.clone(), model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(), project: None,
            kind: hya_proto::SessionKind::Project,
        }).await.unwrap();
        for fail in [false, true] {
            let run = st.start_run(session).unwrap();
            if fail {
                engine.delete_session(session).await.unwrap();
            }
            st.admission_reply_hook = Some(Arc::new({
                let engine = engine.clone();
                let runs = st.runs.clone();
                move || {
                    assert!(!engine.turn_active(session), "reply retained the restore lease");
                    assert!(!runs.is_busy(session), "reply retained the server run guard");
                }
            }));
            let result = spawn_turn_run(&st, session, run, PreparedTurn {
                agent: (*agent).clone(), guidance: None, external_dirs: Vec::new(), explicit_model: None,
            }, Admission::Restore).await;
            if fail { assert!(result.is_err()); } else {
                assert!(matches!(result, Ok(false)), "{}", result.err().map(|e| e.message).unwrap_or_default());
            }
        }
        app.shutdown().await;
    }
}
