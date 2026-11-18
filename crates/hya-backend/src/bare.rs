//! `hya exec --bare`: the three-primitive kernel alone (ADR-0033) — no
//! session engine, bundles, hooks, or permission checks; the event log only.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use hya_core::kernel::{self, StoreEmit};
use hya_proto::{
    AgentName, Event, FinishReason, MessageId, ModelRef, PartId, PartProjection, Role, SessionId,
    SessionKind,
};
use hya_store::SessionStore;
use hya_tool::{PermissionModel, PermissionPlane, PermissionRules, ToolCtx, deadline};
use tokio_util::sync::CancellationToken;

/// `hya exec --bare` on `db`: always in process; a server holding the
/// database is an error (exit 75), never a route.
pub(crate) async fn exec_entry(
    prompt: String,
    model_override: Option<String>,
    db: &str,
    time_budget: Option<u64>,
) -> anyhow::Result<()> {
    match crate::db_writer::claim(db, "exec").await? {
        crate::db_writer::Writer::Direct(store) => {
            if let Some(seconds) = time_budget {
                deadline::install(Duration::from_secs(seconds));
            }
            exec(prompt, model_override, store).await
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
/// final round's assistant text.
async fn exec(
    prompt: String,
    model_override: Option<String>,
    store: SessionStore,
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
    store
        .append_events(session, events)
        .await
        .context("create the session")?;
    let cancel = CancellationToken::new();
    let publish = |_| {};
    let result = kernel::run(kernel::Run {
        store: &store,
        emit: &StoreEmit {
            store: &store,
            publish: &publish,
        },
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
    store
        .append_events(
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
