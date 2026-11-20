//! `hya exec --bare`: the three-primitive kernel alone (ADR-0033) — no
//! session engine, bundles, hooks, or permission checks; the event log only.

use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use hya_core::CoreError;
use hya_core::kernel::{self, Emit, StoreEmit};
use hya_proto::{
    AgentName, Event, FinishReason, MessageId, ModelRef, PartId, PartProjection, Role, SessionId,
    SessionKind,
};
use hya_store::SessionStore;
use hya_tool::{PermissionModel, PermissionPlane, PermissionRules, ToolCtx, deadline};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::exec_stream::write_envelope;

/// `hya exec --bare` on `db`: always in process; a server holding the
/// database is an error (exit 75), never a route.
pub(crate) async fn exec_entry(
    prompt: String,
    model_override: Option<String>,
    db: &str,
    time_budget: Option<u64>,
    json: bool,
) -> anyhow::Result<()> {
    match crate::db_writer::claim(db, "exec").await? {
        crate::db_writer::Writer::Direct(store) => {
            if let Some(seconds) = time_budget {
                deadline::install(Duration::from_secs(seconds));
            }
            exec(prompt, model_override, store, json).await
        }
        crate::db_writer::Writer::Server(server) => crate::db_writer::exit_unroutable(
            "exec",
            db,
            &server,
            "--bare runs the kernel in process and cannot use a running server",
        ),
    }
}

/// Run `prompt` through the kernel in a new session of `store` and print the
/// final round's assistant text or stream its durable envelopes.
async fn exec(
    prompt: String,
    model_override: Option<String>,
    store: SessionStore,
    json: bool,
) -> anyhow::Result<()> {
    crate::first_run_config_bootstrap(false)?;
    let runtime = hya_app::resolve_runtime(model_override).await;
    let model = ModelRef::new(runtime.model);
    let workdir = std::env::current_dir().context("read the working directory")?;
    let session = SessionId::new();
    let user = MessageId::new();
    let part = PartId::new();
    let message = MessageId::new();
    let mut events = vec![
        Event::SessionCreated {
            session,
            parent: None,
            agent: AgentName::new(kernel::AGENT),
            model: model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: SessionKind::Project,
        },
        Event::MessageStarted {
            session,
            message: user,
            role: Role::User,
            agent: None,
            model: None,
        },
        Event::TextStart {
            session,
            message: user,
            part,
        },
        Event::TextDelta {
            session,
            message: user,
            part,
            delta: prompt,
        },
        Event::TextEnd {
            session,
            message: user,
            part,
        },
        Event::UserPromptContextRecorded {
            session,
            message: user,
            files: Vec::new(),
            agents: Vec::new(),
        },
        Event::MessageFinished {
            session,
            message: user,
            role: Role::User,
            finish: FinishReason::Stop,
            tokens: None,
            cause: None,
        },
    ];
    if let Some(deadline) = deadline::current() {
        events.extend(kernel::round::system_message_events(
            session,
            MessageId::new(),
            deadline.opening(),
        ));
    }
    events.push(Event::MessageStarted {
        session,
        message,
        role: Role::Assistant,
        agent: Some(AgentName::new(kernel::AGENT)),
        model: Some(model.clone()),
    });
    let store_emit = StoreEmit {
        store: &store,
        publish: &|_| {},
    };
    let json_emit = JsonEmit {
        store: &store,
        out: Mutex::new(Ok(io::stdout())),
    };
    let emit: &dyn Emit = if json { &json_emit } else { &store_emit };
    emit.emit(session, events)
        .await
        .context("create the session")?;
    let cancel = CancellationToken::new();
    let result = kernel::run(kernel::Run {
        store: &store,
        emit,
        provider: &runtime.router,
        model: &model,
        system: &kernel::prompt::system_prompt(&workdir),
        tools: &kernel::tools::builtin(),
        ctx: &tool_ctx(&workdir, &cancel),
        session,
        message,
        cancel: &cancel,
    })
    .await;
    emit.emit(
        session,
        vec![Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: *result.as_ref().unwrap_or(&FinishReason::Error),
            tokens: None,
            cause: None,
        }],
    )
    .await
    .context("finish the assistant message")?;
    result.context("kernel run")?;
    if json {
        return Ok(());
    }
    let projection = store.read_projection_shared(session).await?;
    let reply = projection
        .session
        .messages
        .iter()
        .find(|m| m.id == message)
        .context("assistant message missing from the projection")?;
    let mut text = String::new();
    for part in &reply.parts[reply.last_step_start.unwrap_or(0)..] {
        if let PartProjection::Text { text: body, .. } = part {
            text.push_str(body);
        }
    }
    println!("{text}");
    Ok(())
}

struct JsonEmit<'a, W> {
    store: &'a SessionStore,
    out: Mutex<Result<W, String>>,
}

#[async_trait::async_trait]
impl<W: Write + Send> Emit for JsonEmit<'_, W> {
    async fn emit(&self, session: SessionId, events: Vec<Event>) -> Result<(), CoreError> {
        let mut out = self.out.lock().await;
        // The kernel can try to record a failure after an emit error. Keep the
        // original error and avoid committing more events without a reader.
        let writer = out
            .as_mut()
            .map_err(|error| CoreError::Invalid(error.clone()))?;
        for envelope in self.store.append_events(session, events).await? {
            if let Err(error) = write_envelope(writer, &envelope).and_then(|()| writer.flush()) {
                let error = format!("write session event to stdout: {error}");
                *out = Err(error.clone());
                return Err(CoreError::Invalid(error));
            }
        }
        Ok(())
    }

    fn live(&self, _event: Event) {}
}

/// Tool context with the planes `SessionEngine::new` uses when nothing is
/// wired, and a permission plane that allows everything.
fn tool_ctx(workdir: &Path, cancel: &CancellationToken) -> ToolCtx {
    let permission = PermissionPlane::new(PermissionRules::new(Vec::new()))
        .0
        .with_invocation_model(PermissionModel::Danger);
    ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction: hya_tool::InteractionPlane::new().0,
        spawner: hya_tool::SpawnerPlane::new().0,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: None,
        parent_session: None,
        skills: hya_tool::SkillPlane::default(),
        artifacts: hya_tool::handle::ArtifactPlane::default(),
        agents: Arc::default(),
        websearch: hya_tool::WebSearchPlane::default(),
        lsp: hya_tool::LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        workdir: workdir.to_path_buf(),
        roots: vec![workdir.to_path_buf()],
        cancel: cancel.clone(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use hya_proto::Envelope;
    use std::io::BufWriter;

    #[tokio::test]
    async fn json_emit_flushes_committed_event_before_next_append() {
        let store = SessionStore::connect_memory().await.unwrap();
        let session = SessionId::new();
        let emit = JsonEmit {
            store: &store,
            out: Mutex::new(Ok(BufWriter::new(Vec::new()))),
        };
        emit.emit(
            session,
            vec![Event::SessionCreated {
                session,
                parent: None,
                agent: kernel::AGENT.into(),
                model: "hya/offline".into(),
                workdir: ".".into(),
                project: None,
                kind: SessionKind::Project,
            }],
        )
        .await
        .unwrap();

        let out = emit.out.lock().await;
        let written = out.as_ref().unwrap().get_ref();
        let emitted: Envelope = serde_json::from_slice(written).unwrap();
        let committed = store.replay(session).await.unwrap();
        assert_eq!(committed.len(), 1);
        assert_eq!(
            serde_json::to_value(emitted).unwrap(),
            serde_json::to_value(&committed[0]).unwrap()
        );
        drop(out);

        emit.emit(
            session,
            vec![Event::MessageStarted {
                session,
                message: MessageId::new(),
                role: Role::User,
                agent: None,
                model: None,
            }],
        )
        .await
        .unwrap();
    }
}
