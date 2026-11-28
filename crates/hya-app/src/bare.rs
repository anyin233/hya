//! Kernel-only runtime composition for bare runs.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use hya_core::CoreError;
use hya_core::kernel::{self, Emit, StoreEmit};
use hya_proto::{
    AgentName, Envelope, Event, FinishReason, MessageId, ModelRef, PartId, PartProjection, Role,
    SessionId, SessionKind,
};
use hya_store::SessionStore;
use hya_tool::{PermissionModel, PermissionPlane, PermissionRules, ToolCtx, deadline};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Receives each durable bare-run envelope in commit order. An error stops output
/// and prevents subsequent event batches from being committed.
pub type BareEventSink<'a> = dyn FnMut(&Envelope) -> Result<(), String> + Send + 'a;

/// Start a new kernel-only session in the current directory with the resolved model.
///
/// With an event sink, return `None` and deliver the committed envelopes to
/// the sink. Without one, return the last assistant step's text.
/// A host may install a deadline before starting the run.
///
/// # Errors
/// Returns working-directory, storage, kernel, or sink failures.
pub async fn run_bare(
    prompt: String,
    model_override: Option<String>,
    store: &SessionStore,
    sink: Option<&mut BareEventSink<'_>>,
    cancel: &CancellationToken,
) -> anyhow::Result<Option<String>> {
    let runtime = crate::resolve_runtime(model_override).await;
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
        store,
        publish: &|_| {},
    };
    let sink_emit = sink.map(|sink| BareEmit {
        store,
        sink: Mutex::new(Ok(sink)),
    });
    let emit: &dyn Emit = match &sink_emit {
        Some(emit) => emit,
        None => &store_emit,
    };
    emit.emit(session, events)
        .await
        .context("create the session")?;
    let bare_tools = kernel::tools::bare_tools();
    let system = kernel::prompt::compose(
        &workdir,
        |_| true,
        &[(kernel::tools::SKILL, kernel::tools::SKILL_PROMPT)],
        None,
    );
    let result = kernel::run(kernel::Run {
        store,
        emit,
        provider: &runtime.router,
        model: &model,
        system: &system,
        tools: &bare_tools,
        ctx: &tool_ctx(&workdir, cancel),
        session,
        message,
        cancel,
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
    if sink_emit.is_some() {
        return Ok(None);
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
    Ok(Some(text))
}

struct BareEmit<'a, 'sink> {
    store: &'a SessionStore,
    sink: Mutex<Result<&'a mut BareEventSink<'sink>, String>>,
}

#[async_trait::async_trait]
impl Emit for BareEmit<'_, '_> {
    async fn emit(&self, session: SessionId, events: Vec<Event>) -> Result<(), CoreError> {
        let mut sink = self.sink.lock().await;
        // A failed sink may already have received part of the committed batch.
        // Keep the original error and block any further appends.
        let write = sink
            .as_mut()
            .map_err(|error| CoreError::Invalid(error.clone()))?;
        for envelope in self.store.append_events(session, events).await? {
            if let Err(error) = write(&envelope) {
                *sink = Err(error.clone());
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
        skills: hya_tool::SkillPlane::from_snapshot(Arc::new(
            hya_engine::skill_catalog::discover_skills(workdir),
        )),
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn bare_sink_failure_keeps_commits_and_blocks_new_appends() -> anyhow::Result<()> {
        let store = SessionStore::connect_memory().await?;
        let session = SessionId::new();
        let mut output = Vec::new();
        let mut sink = |envelope: &Envelope| {
            if output.is_empty() {
                output.push(envelope.clone());
                Ok(())
            } else {
                Err("sink closed".to_string())
            }
        };
        let emit = BareEmit {
            store: &store,
            sink: Mutex::new(Ok(&mut sink)),
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
        .await?;
        let start = Event::MessageStarted {
            session,
            message: MessageId::new(),
            role: Role::User,
            agent: None,
            model: None,
        };
        for event in [start.clone(), start] {
            assert!(matches!(
                emit.emit(session, vec![event]).await,
                Err(CoreError::Invalid(error)) if error == "sink closed"
            ));
        }
        drop(emit);
        let committed = store.replay(session).await?;
        assert_eq!(committed.len(), 2);
        assert_eq!(output, committed[..1]);
        Ok(())
    }
}
