//! Turn end invariants (graceful drain, restart restore, user cancel, lead
//! failure broadcast, archive/interrupt, partial flush, sidecar loss) and
//! resident crash recovery & claim fencing.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt as _, stream};
use hya_engine::{
    AgentSpec, BoundSidecarFactory, CoreError, CreateSession, EventBus, ResidentRecovery,
    ResidentSupervisor, RuntimeCatalogRefresh, RuntimeRegistry, SessionEngine, SidecarEnvironment,
    SidecarHandle, SidecarStart, SubagentGovernor, SubagentLimits, TurnBinding,
};
use hya_proto::{
    AgentName, ArchiveReason, Envelope, Event, FinishCause, FinishReason, MailEndpoint, MailKind,
    MemberId, MemberRunStatus, MessageId, ModelRef, PartId, PartProjection, Role, RosterStatus,
    SessionId, SubagentMode, ToolCallId, ToolName, ToolPartState, ToolSchema,
};
use hya_provider::{
    Capabilities, CompletionRequest, DevProvider, EventStream, FakeProvider, FakeStep, Provider,
    ProviderError, ProviderRouter,
};
use hya_store::{OwnerRunId, SessionStore, StoreError};
use hya_tool::{PermissionPlane, PermissionRules, Tool, ToolCtx, ToolError, ToolRegistry};
use serde_json::json;
use sqlx::{Connection, SqliteConnection};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
enum Script {
    PartialText,
    Write,
    Hang,
    Gate,
    Fail,
    Tool,
}

#[derive(Default)]
struct ScriptProvider {
    scripts: Mutex<HashMap<SessionId, Script>>,
    entered: Notify,
    release: Notify,
    streams: Mutex<HashMap<SessionId, usize>>,
}

impl ScriptProvider {
    fn script(&self, session: SessionId, script: Script) {
        self.scripts.lock().unwrap().insert(session, script);
    }

    fn streams_for(&self, session: SessionId) -> usize {
        self.streams
            .lock()
            .unwrap()
            .get(&session)
            .copied()
            .unwrap_or(0)
    }
}

fn text_answer(session: SessionId, message: MessageId) -> EventStream {
    let events = FakeProvider::materialize(
        &[
            FakeStep::Text("ok".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        session,
        message,
    );
    Box::pin(stream::iter(events.into_iter().map(Ok)))
}

#[async_trait]
impl Provider for ScriptProvider {
    fn id(&self) -> &str {
        "hya"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "hya/offline").then_some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        *self.streams.lock().unwrap().entry(session).or_default() += 1;
        let script = self.scripts.lock().unwrap().remove(&session);
        match script {
            Some(Script::Write) => Ok(Box::pin(stream::iter(
                FakeProvider::materialize(
                    &[
                        FakeStep::ToolCall {
                            name: "write".into(),
                            input: json!({"path": "partial.txt", "content": "saved"}),
                        },
                        FakeStep::Finish(FinishReason::ToolCalls),
                    ],
                    session,
                    message,
                )
                .into_iter()
                .map(Ok),
            ))),
            Some(Script::PartialText) => {
                let (part, reasoning) = (PartId::new(), PartId::new());
                Ok(Box::pin(
                    stream::iter(vec![
                        Ok(Event::ReasoningStart {
                            reason: None,
                            session,
                            message,
                            part: reasoning,
                        }),
                        Ok(Event::ReasoningDelta {
                            session,
                            message,
                            part: reasoning,
                            delta: "thinking".into(),
                        }),
                        Ok(Event::TextStart {
                            session,
                            message,
                            part,
                        }),
                        Ok(Event::TextDelta {
                            session,
                            message,
                            part,
                            delta: "partial answer".into(),
                        }),
                    ])
                    .chain(stream::pending()),
                ))
            }
            Some(Script::Hang) => {
                self.entered.notify_one();
                let open = Event::ToolInputStart {
                    session,
                    message,
                    part: PartId::new(),
                    call: ToolCallId::new(),
                    name: ToolName::new("bash"),
                };
                Ok(Box::pin(
                    stream::iter(vec![Ok(open)]).chain(stream::pending()),
                ))
            }
            Some(Script::Fail) => Err(ProviderError::Decode("upstream exploded".into())),
            Some(Script::Gate) => {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(text_answer(session, message))
            }
            Some(Script::Tool) => {
                let events = FakeProvider::materialize(
                    &[
                        FakeStep::ToolCall {
                            name: "gate_tool".into(),
                            input: json!({}),
                        },
                        FakeStep::Finish(FinishReason::ToolCalls),
                    ],
                    session,
                    message,
                );
                Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
            }
            None => Ok(text_answer(session, message)),
        }
    }
}

struct GatedTool {
    release: Notify,
}

#[async_trait]
impl Tool for GatedTool {
    fn name(&self) -> &str {
        "gate_tool"
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("gate_tool"),
            description: "gated marker".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }
    async fn execute(
        &self,
        _: &ToolCtx,
        _: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        self.release.notified().await;
        Ok(json!({ "ok": true }))
    }
}

struct ShutdownErrorShell {
    entered: Notify,
}

#[async_trait]
impl Tool for ShutdownErrorShell {
    fn name(&self) -> &str {
        "bash"
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("bash"),
            description: "shutdown shell".into(),
            input_schema: json!({"type":"object"}),
            output_schema: None,
        }
    }
    async fn execute(
        &self,
        ctx: &ToolCtx,
        _: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        self.entered.notify_one();
        ctx.cancel.cancelled().await;
        Err(ToolError::Io(std::io::Error::other(
            "child killed by shutdown",
        )))
    }
}

struct WriteAndCancel(&'static str);

#[async_trait]
impl Tool for WriteAndCancel {
    fn name(&self) -> &str {
        self.0
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.0),
            description: "complete at cancellation".into(),
            input_schema: json!({"type":"object"}),
            output_schema: None,
        }
    }
    async fn execute(
        &self,
        ctx: &ToolCtx,
        _: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        tokio::fs::write(ctx.workdir.join("partial.txt"), "saved").await?;
        ctx.cancel.cancel();
        Ok(json!({"output": "completed"}))
    }
}

struct WriteThenWait {
    written: Notify,
}

#[async_trait]
impl Tool for WriteThenWait {
    fn name(&self) -> &str {
        "write"
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("write"),
            description: "write and wait".into(),
            input_schema: json!({"type":"object"}),
            output_schema: None,
        }
    }
    async fn execute(
        &self,
        ctx: &ToolCtx,
        _: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        tokio::fs::write(ctx.workdir.join("partial.txt"), "saved").await?;
        self.written.notify_one();
        std::future::pending().await
    }
}

#[derive(Default)]
struct BindGate {
    entered: Notify,
    release: Notify,
    used: AtomicBool,
}

#[async_trait]
impl RuntimeCatalogRefresh for BindGate {
    async fn refresh_if_changed(&self, _: &RuntimeRegistry) -> Result<bool, CoreError> {
        if !self.used.swap(true, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(false)
    }
}

#[derive(Clone, Default)]
struct StartingSidecar {
    entered: Arc<Notify>,
    release_cleanup: Arc<Notify>,
}

impl SidecarEnvironment for StartingSidecar {
    fn factory_for(
        &self,
        _: &TurnBinding,
        _: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        Ok(Some(Arc::new(self.clone())))
    }
}

#[async_trait]
impl BoundSidecarFactory for StartingSidecar {
    async fn start(&self, _: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait]
impl SidecarHandle for StartingSidecar {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.entered.notify_one();
        std::future::pending().await
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.release_cleanup.notified().await;
        Ok(())
    }
}

#[derive(Clone)]
struct LossSidecar {
    loss: CancellationToken,
    terminated: Arc<Notify>,
    hooks: bool,
}

#[async_trait]
impl BoundSidecarFactory for LossSidecar {
    async fn start(&self, _: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait]
impl SidecarHandle for LossSidecar {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
    fn is_healthy(&self) -> bool {
        !self.loss.is_cancelled()
    }
    fn loss_token(&self) -> Option<CancellationToken> {
        Some(self.loss.clone())
    }
    fn hook_dispatcher(&self) -> Option<Arc<dyn hya_engine::HookDispatcher>> {
        self.hooks
            .then(|| Arc::new(hya_engine::NoopHookHost) as Arc<dyn hya_engine::HookDispatcher>)
    }
    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.terminated.notify_one();
        Ok(())
    }
}

#[derive(Default)]
struct BoundaryHooks {
    bind_pause: Option<(Arc<Notify>, Arc<Notify>)>,
    after_calls: AtomicUsize,
}

#[async_trait]
impl hya_engine::HookDispatcher for BoundaryHooks {
    fn dispatch_event(&self, _: &Envelope) {}
    async fn session_start(&self, _: hya_engine::SessionLifecycleInput) {
        if let Some((entered, release)) = &self.bind_pause {
            entered.notify_one();
            release.notified().await;
        }
    }
    async fn command_execute_before(
        &self,
        i: hya_engine::CommandExecuteBeforeInput,
    ) -> hya_engine::CommandExecuteBeforeOutcome {
        hya_engine::CommandExecuteBeforeOutcome::Continue { text: i.text }
    }
    async fn text_complete(
        &self,
        i: hya_engine::TextCompleteInput,
    ) -> hya_engine::TextCompleteOutcome {
        hya_engine::TextCompleteOutcome::Continue { text: i.text }
    }
    async fn message_user_before(
        &self,
        i: hya_engine::MessageUserBeforeInput,
    ) -> hya_engine::MessageUserBeforeOutcome {
        hya_engine::MessageUserBeforeOutcome::Continue { text: i.text }
    }
    async fn chat_params(&self, i: hya_engine::ChatParamsInput) -> hya_engine::ChatParamsOutcome {
        hya_engine::ChatParamsOutcome::Continue { request: i.request }
    }
    async fn tool_execute_before(
        &self,
        i: hya_engine::ToolExecuteBeforeInput,
    ) -> hya_engine::ToolExecuteBeforeOutcome {
        hya_engine::ToolExecuteBeforeOutcome::Continue { input: i.input }
    }
    async fn tool_execute_after(
        &self,
        _: hya_engine::ToolExecuteAfterInput,
    ) -> hya_engine::ToolExecuteAfterOutcome {
        self.after_calls.fetch_add(1, Ordering::SeqCst);
        hya_engine::ToolExecuteAfterOutcome::Continue {
            result: hya_engine::ToolOutcomeNative::Ok {
                output: json!({"rewritten": true}),
                time_ms: 0,
            },
        }
    }
}

async fn scripted_engine_with_perm(
    provider: Arc<ScriptProvider>,
    tools: Arc<ToolRegistry>,
    perm: PermissionPlane,
) -> (Arc<SessionEngine>, AgentSpec) {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let engine = Arc::new(
        SessionEngine::new(
            store,
            Arc::new(ProviderRouter::new().with(provider)),
            support::test_runtime(tools),
            perm,
            EventBus::default(),
        )
        .with_runtime_owner(owner)
        .with_governor(SubagentGovernor::new(SubagentLimits::default())),
    );
    let mut agent = support::agent_spec("hya-task", "hya/offline", "/tmp");
    agent.system_prompt = "x".into();
    (engine, agent)
}

async fn engine_with_script() -> (Arc<SessionEngine>, AgentSpec, Arc<ScriptProvider>) {
    let provider = Arc::new(ScriptProvider::default());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let (engine, agent) = scripted_engine_with_perm(
        provider.clone(),
        Arc::new(hya_tool_loader::builtins()),
        perm,
    )
    .await;
    (engine, agent, provider)
}

fn next_engine(engine: &SessionEngine, provider: Arc<ScriptProvider>) -> SessionEngine {
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(
        engine.store().clone(),
        Arc::new(ProviderRouter::new().with(provider)),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        perm,
        EventBus::default(),
    )
    .with_runtime_owner(engine.runtime_owner())
}

async fn make_session(engine: &SessionEngine, parent: Option<SessionId>, agent: &str) -> SessionId {
    engine
        .create(CreateSession {
            parent,
            agent: AgentName::new(agent),
            model: ModelRef::new("hya/offline"),
            workdir: "/tmp".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap()
}

async fn register_main(
    supervisor: &ResidentSupervisor,
    engine: &SessionEngine,
    agent: &AgentSpec,
    root: SessionId,
) {
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let agents = engine
        .agent_roster_for_binding(&binding, "hya-main")
        .unwrap();
    let resources = engine
        .agent_resource_policy_for_binding(&binding, "hya-main")
        .unwrap();
    supervisor
        .ensure_main(
            root,
            agent.clone(),
            (binding, agents, resources),
            None,
            None,
        )
        .await
        .unwrap();
}

async fn run_prompt(
    engine: &SessionEngine,
    session: SessionId,
    agent: &AgentSpec,
    prompt: MessageId,
) -> Result<FinishReason, CoreError> {
    engine
        .run_turn_with_external_dirs_and_guidance(
            session,
            agent,
            CancellationToken::new(),
            &[],
            None,
            None,
            Some(prompt),
        )
        .await
}

type Finishes = Vec<(FinishReason, Option<FinishCause>)>;

fn assistant_finishes(events: &[Envelope], session: SessionId) -> Vec<(MessageId, Finishes)> {
    let mut order = Vec::new();
    let mut finishes: HashMap<MessageId, Finishes> = HashMap::new();
    for env in events {
        match &env.event {
            Event::MessageStarted {
                session: s,
                message,
                role: Role::Assistant,
                ..
            } if *s == session => {
                order.push(*message);
                finishes.entry(*message).or_default();
            }
            Event::MessageFinished {
                session: s,
                message,
                role: Role::Assistant,
                finish,
                cause,
                ..
            } if *s == session => {
                finishes
                    .entry(*message)
                    .or_default()
                    .push((*finish, *cause));
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .map(|m| (m, finishes.remove(&m).unwrap_or_default()))
        .collect()
}

async fn open_tool_parts(engine: &SessionEngine, session: SessionId) -> usize {
    engine
        .read_projection(session)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .flat_map(|m| &m.parts)
        .filter(|p| {
            matches!(
                p,
                PartProjection::Tool {
                    state: ToolPartState::Pending { .. } | ToolPartState::Running { .. },
                    ..
                }
            )
        })
        .count()
}

async fn roster_status(
    engine: &SessionEngine,
    root: SessionId,
    leaf: &str,
) -> Option<RosterStatus> {
    engine
        .read_projection(root)
        .await
        .unwrap()
        .team
        .roster
        .values()
        .find(|e| hya_proto::scope::leaf(&e.handle) == leaf)
        .map(|e| e.status)
}

async fn harness_mail_to(engine: &SessionEngine, root: SessionId, leaf: &str) -> Vec<String> {
    let team = engine.read_projection(root).await.unwrap().team;
    team.inboxes
        .get(&format!("main/{leaf}"))
        .map(|inbox| {
            inbox
                .iter()
                .map(|i| &team.mail[*i])
                .filter(|m| m.from == hya_proto::HARNESS_HANDLE)
                .map(|m| m.body.clone())
                .collect()
        })
        .unwrap_or_default()
}

async fn restore_prompt_count(engine: &SessionEngine, session: SessionId) -> usize {
    engine
        .replay(session)
        .await
        .unwrap()
        .iter()
        .filter(|e| matches!(&e.event, Event::TextDelta { delta, .. } if delta.contains("Your previous turn was interrupted by a restart.")))
        .count()
}

async fn pending_prompt(engine: &SessionEngine, session: SessionId) -> String {
    engine
        .store()
        .take_restart_resume(session, engine.runtime_owner())
        .await
        .unwrap()
        .unwrap()
        .1
        .into_iter()
        .find_map(|e| match e.event {
            Event::TextDelta { delta, .. } => Some(delta),
            _ => None,
        })
        .unwrap()
}

async fn stop_during_bind(
    engine: Arc<SessionEngine>,
    agent: AgentSpec,
    session: SessionId,
    prompt: MessageId,
    gate: Arc<BindGate>,
) {
    let turn = {
        let e = engine.clone();
        tokio::spawn(async move { run_prompt(&e, session, &agent, prompt).await })
    };
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    let stop = {
        let e = engine.clone();
        tokio::spawn(async move { e.stop_for_restart(Duration::from_secs(5)).await })
    };
    assert!(
        support::poll_until(Duration::from_secs(2), || async {
            engine.draining().is_some()
        })
        .await
    );
    gate.release.notify_one();
    assert!(stop.await.unwrap().unwrap().stragglers.is_empty());
    assert_eq!(turn.await.unwrap().unwrap(), FinishReason::Cancelled);
}

async fn stop_mid_turn(
    engine: Arc<SessionEngine>,
    agent: AgentSpec,
    session: SessionId,
    provider: &ScriptProvider,
) {
    provider.script(session, Script::Hang);
    let turn = {
        let e = engine.clone();
        tokio::spawn(async move { e.run_turn(session, &agent, CancellationToken::new()).await })
    };
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            open_tool_parts(&engine, session).await == 1
        })
        .await
    );
    engine
        .stop_for_restart(Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(turn.await.unwrap().unwrap(), FinishReason::Cancelled);
}

async fn hanging_member(
    engine: &Arc<SessionEngine>,
    supervisor: &Arc<ResidentSupervisor>,
    provider: &ScriptProvider,
    agent: &AgentSpec,
    root: SessionId,
    handle: &str,
) -> SessionId {
    let member = make_session(engine, Some(root), "hya-task").await;
    provider.script(member, Script::Hang);
    supervisor
        .register_existing_resident(
            root,
            member,
            handle.into(),
            agent.clone(),
            Some("work".into()),
        )
        .await
        .unwrap();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            engine.turn_active(member) && open_tool_parts(engine, member).await == 1
        })
        .await
    );
    member
}

fn tool_event_order(log: &[Envelope]) -> Vec<&Event> {
    log.iter()
        .map(|e| &e.event)
        .filter(|e| {
            matches!(
                e,
                Event::ToolInputStart { .. }
                    | Event::ToolCallRequested { .. }
                    | Event::StepFinished {
                        finish: FinishReason::ToolCalls,
                        ..
                    }
                    | Event::ToolResult { .. }
                    | Event::ToolError { .. }
            )
        })
        .collect()
}

#[tokio::test]
async fn drain_refusal_without_admitted_prompt_and_finished_lease_skip_restore() {
    let (engine, agent, _) = engine_with_script().await;
    let empty = make_session(&engine, None, "hya-main").await;
    let answered = make_session(&engine, None, "hya-main").await;
    let unowned = make_session(&engine, None, "hya-main").await;
    engine
        .admit_user_prompt(unowned, "someone else's".into())
        .await
        .unwrap();
    engine
        .admit_user_prompt(answered, "done".into())
        .await
        .unwrap();
    engine
        .run_turn(answered, &agent, CancellationToken::new())
        .await
        .unwrap();
    let before = engine.replay(answered).await.unwrap();
    let unowned_before = engine.replay(unowned).await.unwrap();
    engine.begin_drain(FinishCause::Shutdown);
    for session in [empty, answered, unowned] {
        assert_eq!(
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await
                .unwrap(),
            FinishReason::Cancelled
        );
    }
    assert!(
        engine
            .read_projection(empty)
            .await
            .unwrap()
            .session
            .messages
            .is_empty()
    );
    assert_eq!(engine.replay(answered).await.unwrap(), before);
    assert_eq!(engine.replay(unowned).await.unwrap(), unowned_before);
    assert!(
        engine
            .store()
            .list_pending_resumes()
            .await
            .unwrap()
            .is_empty()
    );

    // Finished turn still holding its lease does not enqueue a restore on stop.
    let (engine, _, _) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    let lease = engine.try_begin_turn(session).unwrap();
    let msg = MessageId::new();
    for event in [
        Event::MessageStarted {
            session,
            message: msg,
            role: Role::Assistant,
            agent: None,
            model: None,
        },
        Event::MessageFinished {
            session,
            message: msg,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
            cause: None,
        },
    ] {
        engine.store().append_event(session, &event).await.unwrap();
    }
    engine.stop_for_restart(Duration::ZERO).await.unwrap();
    drop(lease);
    assert!(
        engine
            .store()
            .list_pending_resumes()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn restart_prestart_second_stop_crash_and_ordinary_discard() {
    // 1. Stop during pre-start binding restores turn once.
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    let prompt = engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    let gate = Arc::new(BindGate::default());
    let gated = Arc::new(next_engine(&engine, provider.clone()).with_catalog_refresh(gate.clone()));
    stop_during_bind(gated.clone(), agent.clone(), session, prompt, gate).await;
    assert_eq!(gated.store().list_pending_resumes().await.unwrap().len(), 1);
    let next = next_engine(&gated, provider.clone());
    let restore = next.inject_restart_prompt(session).await.unwrap().unwrap();
    assert_eq!(
        run_prompt(&next, session, &agent, restore).await.unwrap(),
        FinishReason::Stop
    );
    assert_eq!(
        (
            restore_prompt_count(&next, session).await,
            provider.streams_for(session)
        ),
        (1, 1)
    );

    // 2. Second stop before restored assistant start is a new interruption.
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    stop_mid_turn(engine.clone(), agent.clone(), session, &provider).await;
    let gate = Arc::new(BindGate::default());
    let second =
        Arc::new(next_engine(&engine, provider.clone()).with_catalog_refresh(gate.clone()));
    let restore = second
        .inject_restart_prompt(session)
        .await
        .unwrap()
        .unwrap();
    stop_during_bind(second.clone(), agent.clone(), session, restore, gate).await;
    let third = next_engine(&second, provider);
    let restore2 = third.inject_restart_prompt(session).await.unwrap().unwrap();
    assert_eq!(
        run_prompt(&third, session, &agent, restore2).await.unwrap(),
        FinishReason::Stop
    );
    assert_eq!(restore_prompt_count(&third, session).await, 2);
    assert!(
        third
            .store()
            .list_pending_resumes()
            .await
            .unwrap()
            .is_empty()
    );

    // 3. Crash after injection reuses the injected prompt without injecting a second.
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    stop_mid_turn(engine.clone(), agent.clone(), session, &provider).await;
    let second = next_engine(&engine, provider.clone());
    let injected = second
        .inject_restart_prompt(session)
        .await
        .unwrap()
        .unwrap();
    drop(second);
    let third = next_engine(&engine, provider.clone());
    assert_eq!(
        third.inject_restart_prompt(session).await.unwrap(),
        Some(injected)
    );
    assert_eq!(restore_prompt_count(&third, session).await, 1);
    assert_eq!(
        run_prompt(&third, session, &agent, injected).await.unwrap(),
        FinishReason::Stop
    );
    assert!(
        third
            .store()
            .list_pending_resumes()
            .await
            .unwrap()
            .is_empty()
    );

    // 4. Ordinary turn discards an uninjected restore.
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    stop_mid_turn(engine.clone(), agent.clone(), session, &provider).await;
    let ordinary = next_engine(&engine, provider.clone());
    ordinary
        .admit_user_prompt(session, "continue directly".into())
        .await
        .unwrap();
    ordinary
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        ordinary
            .store()
            .list_pending_resumes()
            .await
            .unwrap()
            .is_empty()
    );
    let next = next_engine(&ordinary, provider);
    assert!(next.inject_restart_prompt(session).await.unwrap().is_none());
}

#[tokio::test]
async fn restart_during_sidecar_shell_binding_queued_and_user_cancel_binding() {
    // 1. Restart during sidecar start (clean + zero-deadline straggler).
    for zero_deadline in [false, true] {
        let (engine, agent, provider) = engine_with_script().await;
        let session = make_session(&engine, None, "hya-main").await;
        let prompt = engine
            .admit_user_prompt(session, "work".into())
            .await
            .unwrap();
        let sidecar = Arc::new(StartingSidecar::default());
        if !zero_deadline {
            sidecar.release_cleanup.notify_one();
        }
        let engine = Arc::new(
            next_engine(&engine, provider.clone()).with_sidecar_environment(sidecar.clone()),
        );
        let turn = {
            let e = engine.clone();
            tokio::spawn(async move { run_prompt(&e, session, &agent, prompt).await })
        };
        tokio::time::timeout(Duration::from_secs(5), sidecar.entered.notified())
            .await
            .unwrap();
        let report = engine
            .stop_for_restart(if zero_deadline {
                Duration::ZERO
            } else {
                Duration::from_secs(5)
            })
            .await
            .unwrap();
        assert_eq!(report.stragglers.is_empty(), !zero_deadline);
        if zero_deadline {
            sidecar.release_cleanup.notify_one();
        }
        assert_eq!(turn.await.unwrap().unwrap(), FinishReason::Cancelled);
        let pending = engine.store().list_pending_resumes().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            assistant_finishes(&engine.replay(session).await.unwrap(), session),
            vec![(
                pending[0].message,
                vec![(FinishReason::Cancelled, Some(FinishCause::Shutdown))]
            )]
        );
        assert_eq!(provider.streams_for(session), 0);
    }

    // 2. Restart during shell binding emits shutdown pair and restore.
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    let gate = Arc::new(BindGate::default());
    let engine = Arc::new(next_engine(&engine, provider).with_catalog_refresh(gate.clone()));
    let turn = {
        let (e, a) = (engine.clone(), agent.clone());
        tokio::spawn(async move {
            e.run_shell(session, &a, "echo unused".into(), CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    assert!(
        engine
            .stop_for_restart(Duration::from_secs(5))
            .await
            .unwrap()
            .stragglers
            .is_empty()
    );
    assert_eq!(turn.await.unwrap().unwrap().1, FinishReason::Cancelled);
    assert_eq!(
        engine.store().list_pending_resumes().await.unwrap().len(),
        1
    );

    // 3. User cancel during binding keeps UserCancel finish without queueing restore.
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    let prompt = engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    let gate = Arc::new(BindGate::default());
    let engine = Arc::new(next_engine(&engine, provider).with_catalog_refresh(gate.clone()));
    let turn = {
        let (e, a) = (engine.clone(), agent.clone());
        tokio::spawn(async move { run_prompt(&e, session, &a, prompt).await })
    };
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    assert!(engine.cancel_turn(session, FinishCause::UserCancel));
    gate.release.notify_one();
    assert_eq!(turn.await.unwrap().unwrap(), FinishReason::Cancelled);
    assert_eq!(
        assistant_finishes(&engine.replay(session).await.unwrap(), session)[0].1,
        vec![(FinishReason::Cancelled, Some(FinishCause::UserCancel))]
    );
    assert!(
        engine
            .store()
            .list_pending_resumes()
            .await
            .unwrap()
            .is_empty()
    );

    // 4. Drain while queued behind an active lease records the interruption.
    let (engine, agent, _) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    let prompt = engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    let lease = engine.try_begin_turn(session).unwrap();
    let turn = run_prompt(&engine, session, &agent, prompt);
    tokio::pin!(turn);
    assert!(futures::poll!(&mut turn).is_pending());
    engine.begin_drain(FinishCause::Shutdown);
    drop(lease);
    assert_eq!(turn.await.unwrap(), FinishReason::Cancelled);
    assert_eq!(
        engine.store().list_pending_resumes().await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn drain_closes_every_session_turn_with_cause_and_archives_members() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    provider.script(root, Script::Hang);
    engine.admit_user_prompt(root, "lead".into()).await.unwrap();
    let lead = {
        let (e, a) = (engine.clone(), agent.clone());
        tokio::spawn(async move { e.run_turn(root, &a, CancellationToken::new()).await })
    };
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .unwrap();
    register_main(&supervisor, &engine, &agent, root).await;
    let a = hanging_member(&engine, &supervisor, &provider, &agent, root, "worker-a").await;
    let b = hanging_member(&engine, &supervisor, &provider, &agent, root, "worker-b").await;
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            open_tool_parts(&engine, root).await == 1
        })
        .await
    );

    let report = supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
    assert!(report.stragglers.is_empty());
    assert_eq!(
        report.cancelled.iter().copied().collect::<HashSet<_>>(),
        HashSet::from([root, a, b])
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), lead)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        FinishReason::Cancelled
    );

    for session in [root, a, b] {
        let finishes = assistant_finishes(&engine.replay(session).await.unwrap(), session);
        assert_eq!(
            finishes[0].1,
            vec![(FinishReason::Cancelled, Some(FinishCause::Shutdown))]
        );
        assert_eq!(open_tool_parts(&engine, session).await, 0);
        assert!(!engine.turn_active(session));
    }
    let team = engine.read_projection(root).await.unwrap().team;
    for leaf in ["worker-a", "worker-b"] {
        assert_eq!(
            team.archived[&format!("main/{leaf}")].reason,
            ArchiveReason::Shutdown
        );
        assert!(!team.roster.contains_key(&format!("main/{leaf}")));
    }
    assert!(roster_status(&engine, root, "main").await.is_some());

    let prompt = engine
        .admit_user_prompt(root, "again".into())
        .await
        .unwrap();
    assert_eq!(
        run_prompt(&engine, root, &agent, prompt).await.unwrap(),
        FinishReason::Cancelled
    );
    assert_eq!(provider.streams_for(root), 1);
}

#[tokio::test]
async fn user_cancel_does_not_broadcast_while_lead_failure_broadcasts_wrap_up() {
    // 1. User cancel records UserCancel and sends no leader-failed mail.
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let scout = make_session(&engine, Some(root), "hya-task").await;
    supervisor
        .register_existing_resident(root, scout, "scout".into(), agent.clone(), None)
        .await
        .unwrap();
    provider.script(root, Script::Hang);
    engine.admit_user_prompt(root, "lead".into()).await.unwrap();
    let lead = {
        let (e, a) = (engine.clone(), agent.clone());
        tokio::spawn(async move { e.run_turn(root, &a, CancellationToken::new()).await })
    };
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            open_tool_parts(&engine, root).await == 1
        })
        .await
    );
    assert!(engine.cancel_turn(root, FinishCause::UserCancel));
    assert_eq!(lead.await.unwrap().unwrap(), FinishReason::Cancelled);
    assert_eq!(
        assistant_finishes(&engine.replay(root).await.unwrap(), root)[0].1,
        vec![(FinishReason::Cancelled, Some(FinishCause::UserCancel))]
    );
    assert_eq!(open_tool_parts(&engine, root).await, 0);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            !engine.turn_active(root)
        })
        .await
    );
    assert!(harness_mail_to(&engine, root, "scout").await.is_empty());
    assert!(!engine.cancel_turn(root, FinishCause::UserCancel));

    // 2. Direct lead provider failure broadcasts LEADER FAILED, never archives lead, and stays resumable.
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "hya-task").await;
    provider.script(worker, Script::Gate);
    supervisor
        .register_existing_resident(
            root,
            worker,
            "worker".into(),
            agent.clone(),
            Some("build".into()),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .unwrap();
    provider.script(root, Script::Fail);
    engine.admit_user_prompt(root, "lead".into()).await.unwrap();
    assert!(
        engine
            .run_turn(root, &agent, CancellationToken::new())
            .await
            .is_err()
    );
    assert_eq!(
        assistant_finishes(&engine.replay(root).await.unwrap(), root)[0].1,
        vec![(FinishReason::Error, Some(FinishCause::ProviderError))]
    );
    assert!(harness_mail_to(&engine, root, "worker").await[0].contains("LEADER FAILED"));
    provider.release.notify_one();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            provider.streams_for(worker) >= 2
                && roster_status(&engine, root, "worker").await == Some(RosterStatus::Idle)
        })
        .await
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(provider.streams_for(root), 1);
    assert!(roster_status(&engine, root, "main").await.is_some());
    engine
        .admit_user_prompt(root, "resume".into())
        .await
        .unwrap();
    assert_eq!(
        engine
            .run_turn(root, &agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );

    // 3. Failed harness-woken lead turn does not archive main either.
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "hya-task").await;
    supervisor
        .register_existing_resident(root, worker, "worker".into(), agent.clone(), None)
        .await
        .unwrap();
    provider.script(root, Script::Fail);
    engine
        .mail_send(
            worker,
            MailEndpoint::Handle("main".into()),
            MailKind::Message,
            "halfway".into(),
        )
        .await
        .unwrap();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            !harness_mail_to(&engine, root, "worker").await.is_empty()
                && !assistant_finishes(&engine.replay(root).await.unwrap(), root).is_empty()
        })
        .await
    );
    assert!(roster_status(&engine, root, "main").await.is_some());
    assert_eq!(provider.streams_for(root), 1);
}

#[tokio::test]
async fn archive_member_and_user_archive_cancel_busy_turn_with_cause_archived() {
    for via_user_api in [false, true] {
        let (engine, agent, provider) = engine_with_script().await;
        let root = make_session(&engine, None, "hya-main").await;
        let supervisor = ResidentSupervisor::start(engine.clone());
        register_main(&supervisor, &engine, &agent, root).await;
        let worker = hanging_member(&engine, &supervisor, &provider, &agent, root, "worker").await;

        let receipt = if via_user_api {
            assert!(
                engine
                    .archive_subagent(root)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("not a subagent")
            );
            engine.archive_subagent(worker).await.unwrap()
        } else {
            supervisor
                .archive_member(root, "worker", "no longer needed")
                .await
                .unwrap()
        };
        assert_eq!(
            (
                receipt.handle.as_str(),
                receipt.session,
                receipt.cancelled_turn
            ),
            ("main/worker", worker, true)
        );
        assert!(!engine.turn_active(worker));
        assert_eq!(
            assistant_finishes(&engine.replay(worker).await.unwrap(), worker)[0].1,
            vec![(FinishReason::Cancelled, Some(FinishCause::Archived))]
        );
        assert_eq!(open_tool_parts(&engine, worker).await, 0);
        let team = engine.read_projection(root).await.unwrap().team;
        assert!(!team.roster.contains_key("main/worker"));
        assert_eq!(
            team.archived["main/worker"].reason,
            ArchiveReason::ArchivedByParent
        );
        assert!(
            !engine
                .store()
                .active_actor_ids()
                .await
                .unwrap()
                .contains(&worker)
        );
    }
}

#[tokio::test]
async fn interrupting_lead_or_subagent_stops_until_user_acts_and_idle_stops_nothing() {
    // 1. Cancelling root stops all subagents, refuses lead mail, and resumes on user action.
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let first = hanging_member(&engine, &supervisor, &provider, &agent, root, "first").await;
    let second = hanging_member(&engine, &supervisor, &provider, &agent, root, "second").await;

    assert!(engine.cancel_user_turn(root));
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            !engine.turn_active(first) && !engine.turn_active(second)
        })
        .await
    );
    for m in [first, second] {
        assert_eq!(
            assistant_finishes(&engine.replay(m).await.unwrap(), m)[0].1,
            vec![(FinishReason::Cancelled, Some(FinishCause::UserCancel))]
        );
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        (
            provider.streams_for(first),
            provider.streams_for(second),
            provider.streams_for(root)
        ),
        (1, 1, 0)
    );
    assert!(
        engine
            .mail_send(
                root,
                MailEndpoint::Handle("main/first".into()),
                MailKind::Message,
                "resume".into()
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("the user stopped `main/first`")
    );
    engine.resume_after_user_stop(root);
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            let team = engine.read_projection(root).await.unwrap().team;
            provider.streams_for(first) == 2
                && provider.streams_for(second) == 2
                && ["main/first", "main/second"].iter().all(|h| {
                    team.roster[*h].status == RosterStatus::Idle
                        && team.roster[*h].resident_work.is_none()
                })
        })
        .await
    );
    // Interrupting an idle member stops nothing; lead mail still wakes it.
    assert!(!engine.cancel_user_turn(first) && !engine.stopped_by_user(first));
    engine
        .mail_send(
            root,
            MailEndpoint::Handle("main/first".into()),
            MailKind::Message,
            "next".into(),
        )
        .await
        .unwrap();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            provider.streams_for(first) == 3
        })
        .await
    );

    // 2. Interrupting one subagent leaves sibling running, refuses lead mail, and resumes on steer_subagent.
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let first = hanging_member(&engine, &supervisor, &provider, &agent, root, "first").await;
    let second = hanging_member(&engine, &supervisor, &provider, &agent, root, "second").await;
    assert!(engine.cancel_user_turn(first));
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            !engine.turn_active(first)
        })
        .await
    );
    assert!(engine.turn_active(second));
    assert!(
        engine
            .mail_send(
                root,
                MailEndpoint::Handle("main/first".into()),
                MailKind::Message,
                "go".into()
            )
            .await
            .is_err()
    );
    engine.steer_subagent(first, "carry on").await.unwrap();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            provider.streams_for(first) == 2
        })
        .await
    );
    assert!(engine.cancel_turn(second, FinishCause::UserCancel));
}

#[tokio::test]
async fn stop_cancels_root_child_tools_and_shell_skipping_after_hooks() {
    // 1. Stop cancels root + child, queues only root with interrupted subagent note.
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    provider.script(root, Script::Hang);
    engine
        .admit_user_prompt(root, "continue".into())
        .await
        .unwrap();
    let task = {
        let (e, a) = (engine.clone(), agent.clone());
        tokio::spawn(async move { e.run_turn(root, &a, CancellationToken::new()).await })
    };
    register_main(&supervisor, &engine, &agent, root).await;
    let _child = hanging_member(&engine, &supervisor, &provider, &agent, root, "worker").await;
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            open_tool_parts(&engine, root).await == 1
        })
        .await
    );
    let report = engine
        .stop_for_restart(Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!((report.cancelled.len(), report.stragglers.len()), (2, 0));
    assert_eq!(task.await.unwrap().unwrap(), FinishReason::Cancelled);
    let pending = engine.store().list_pending_resumes().await.unwrap();
    assert_eq!((pending.len(), pending[0].session), (1, root));
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(2))
        .await;
    let prompt = pending_prompt(&engine, root).await;
    assert!(
        prompt.contains("bash (call ") && prompt.contains("Interrupted subagents: main/worker.")
    );

    // 2. Stop interrupts a running tool without recording ToolResult.
    let provider = Arc::new(ScriptProvider::default());
    let tools = Arc::new(hya_tool_loader::builtins());
    tools
        .register(Arc::new(GatedTool {
            release: Notify::new(),
        }))
        .unwrap();
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let (engine, agent) = scripted_engine_with_perm(provider.clone(), tools, perm).await;
    let session = make_session(&engine, None, "hya-main").await;
    provider.script(session, Script::Tool);
    engine
        .admit_user_prompt(session, "run tool".into())
        .await
        .unwrap();
    let task = {
        let (e, a) = (engine.clone(), agent.clone());
        tokio::spawn(async move { e.run_turn(session, &a, CancellationToken::new()).await })
    };
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            open_tool_parts(&engine, session).await == 1
        })
        .await
    );
    engine
        .stop_for_restart(Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap(), FinishReason::Cancelled);
    assert!(
        pending_prompt(&engine, session)
            .await
            .contains("gate_tool (call ")
    );
    assert!(
        !engine
            .replay(session)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(e.event, Event::ToolResult { .. }))
    );

    // 3. Shutdown IO error in direct shell skips after-hooks and queues shutdown restore.
    let shell = Arc::new(ShutdownErrorShell {
        entered: Notify::new(),
    });
    let tools = Arc::new(hya_tool_loader::builtins());
    tools.remove("bash");
    tools.register(shell.clone()).unwrap();
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let (engine, agent) =
        scripted_engine_with_perm(Arc::new(ScriptProvider::default()), tools, perm).await;
    let hooks = Arc::new(BoundaryHooks::default());
    let engine = Arc::new((*engine).clone().with_hooks(hooks.clone()));
    let session = make_session(&engine, None, "hya-main").await;
    let turn = {
        let (e, a) = (engine.clone(), agent.clone());
        tokio::spawn(async move {
            e.run_shell(session, &a, "wait".into(), CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), shell.entered.notified())
        .await
        .unwrap();
    engine
        .stop_for_restart(Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(turn.await.unwrap().unwrap().1, FinishReason::Cancelled);
    assert_eq!(hooks.after_calls.load(Ordering::SeqCst), 0);
    assert!(
        pending_prompt(&engine, session)
            .await
            .contains("bash (call ")
    );
}

#[tokio::test]
async fn cancel_preserves_partial_text_completed_results_and_written_file_changes() {
    // 1. Cancelling mid-stream flushes partial text and reasoning for every cause.
    for cause in [
        FinishCause::UserCancel,
        FinishCause::Shutdown,
        FinishCause::Other,
    ] {
        let (engine, agent, provider) = engine_with_script().await;
        let session = make_session(&engine, None, "hya-main").await;
        engine
            .admit_user_prompt(session, "answer".into())
            .await
            .unwrap();
        provider.script(session, Script::PartialText);
        let mut live = engine.bus().subscribe();
        let turn = {
            let (e, a) = (engine.clone(), agent.clone());
            tokio::spawn(async move { e.run_turn(session, &a, CancellationToken::new()).await })
        };
        let observed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Event::TextDelta { delta, .. } = live.recv().await.unwrap().event {
                    break delta;
                }
            }
        })
        .await
        .unwrap();
        assert!(engine.cancel_turn(session, cause));
        assert_eq!(turn.await.unwrap().unwrap(), FinishReason::Cancelled);
        let replay = engine.replay(session).await.unwrap();
        assert!(
            replay
                .iter()
                .any(|e| matches!(&e.event, Event::TextReplace { text, .. } if text == &observed))
        );
        assert!(replay.iter().any(
            |e| matches!(&e.event, Event::ReasoningReplace { text, .. } if text == "thinking")
        ));
    }

    // 2. Completed tool & shell results survive simultaneous cancellation.
    for shell in [false, true] {
        let provider = Arc::new(ScriptProvider::default());
        let tools = Arc::new(hya_tool_loader::builtins());
        let name = if shell { "bash" } else { "write" };
        tools.remove(name);
        tools.register(Arc::new(WriteAndCancel(name))).unwrap();
        let (perm, _) = PermissionPlane::new(PermissionRules::default());
        let (engine, mut agent) = scripted_engine_with_perm(provider.clone(), tools, perm).await;
        let dir = support::TestDir::new("completed-cancel");
        if shell {
            assert!(
                std::process::Command::new("git")
                    .arg("init")
                    .arg(dir.path())
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        }
        agent.workdir = dir.path().into();
        let session = support::create_root_session(
            &engine,
            agent.name.as_str(),
            agent.model.as_str(),
            dir.path(),
        )
        .await;
        provider.script(session, Script::Write);
        engine
            .admit_user_prompt(session, "write".into())
            .await
            .unwrap();
        let finish = if shell {
            engine
                .run_shell(session, &agent, "write".into(), CancellationToken::new())
                .await
                .unwrap()
                .1
        } else {
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await
                .unwrap()
        };
        assert_eq!(finish, FinishReason::Cancelled);
        let events = engine.replay(session).await.unwrap();
        assert!(events.iter().any(|e| matches!(&e.event, Event::ToolResult { output, .. } if output["output"] == "completed")));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.event, Event::ToolError { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e.event, Event::FilesChanged { .. }))
        );
    }

    // 3. Cancelled in-flight write tool records FilesChanged before MessageFinished.
    let provider = Arc::new(ScriptProvider::default());
    let tools = Arc::new(hya_tool_loader::builtins());
    tools.remove("write");
    let tool = Arc::new(WriteThenWait {
        written: Notify::new(),
    });
    tools.register(tool.clone()).unwrap();
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let (engine, mut agent) = scripted_engine_with_perm(provider.clone(), tools, perm).await;
    let dir = support::TestDir::new("cancel-written");
    agent.workdir = dir.path().to_path_buf();
    let session =
        support::create_root_session(&engine, "hya-main", "hya/offline", dir.path()).await;
    engine
        .admit_user_prompt(session, "write".into())
        .await
        .unwrap();
    provider.script(session, Script::Write);
    let turn = {
        let (e, a) = (engine.clone(), agent.clone());
        tokio::spawn(async move { e.run_turn(session, &a, CancellationToken::new()).await })
    };
    tokio::time::timeout(Duration::from_secs(5), tool.written.notified())
        .await
        .unwrap();
    engine.cancel_turn(session, FinishCause::UserCancel);
    assert_eq!(turn.await.unwrap().unwrap(), FinishReason::Cancelled);
    let replay = engine.replay(session).await.unwrap();
    assert!(matches!(
        tool_event_order(&replay).as_slice(),
        [Event::ToolInputStart { call, .. }, Event::ToolCallRequested { call: req, .. }, Event::StepFinished { .. }, Event::ToolError { call: res, message_text, value: Some(val), .. }]
            if call == req && call == res && message_text == "cancelled" && val["error"]["type"] == "cancelled"
    ));
    let files_pos = replay
        .iter()
        .position(|e| matches!(e.event, Event::FilesChanged { .. }))
        .unwrap();
    let finish_pos = replay
        .iter()
        .rposition(|e| matches!(e.event, Event::MessageFinished { .. }))
        .unwrap();
    assert!(files_pos < finish_pos);
}

#[tokio::test]
async fn root_mail_and_synthesis_wakes_stopped_during_binding_own_prompt() {
    for mail in [true, false] {
        let (engine, agent, provider) = engine_with_script().await;
        let root = make_session(&engine, None, "hya-main").await;
        let supervisor = ResidentSupervisor::start(engine.clone());
        let binding = engine.bind_runtime(&agent.workdir).unwrap();
        let worker_agent = engine
            .agent_spec_for_binding(&binding, &agent, "hya-task")
            .unwrap();
        let agents = engine
            .agent_roster_for_binding(&binding, "hya-task")
            .unwrap();
        let resources = engine
            .agent_resource_policy_for_binding(&binding, "hya-task")
            .unwrap();
        let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        let hooks = Arc::new(BoundaryHooks {
            bind_pause: Some((entered.clone(), release.clone())),
            ..BoundaryHooks::default()
        });
        let plugin = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
            "wake-bind",
            vec![hya_bundle::SourceFile::new(
                "bundle.yaml",
                "kind: Bundle\nidentity: { id: test/wake-bind, version: 1.0.0, publisher: test }\n",
            )],
        ))
        .unwrap();
        let catalog = hya_engine::AgentCatalog::new(Arc::new(
            hya_bundle::BundleCatalog::from_verified_catalogs(&[&plugin]).unwrap(),
        ))
        .unwrap();
        engine
            .runtime_registry()
            .refresh(|c| {
                c.replace_catalog(Arc::new(catalog));
                c.upsert_sources(vec![
                    hya_engine::RuntimeSource::new(
                        hya_engine::RuntimeSourceId::bundle("test/wake-bind"),
                        [1; 32],
                        Arc::new(()),
                        Vec::new(),
                    )
                    .with_hooks(hooks),
                ])
            })
            .unwrap();
        register_main(&supervisor, &engine, &agent, root).await;
        let (worker, handle) = supervisor
            .spawn_resident(
                root,
                worker_agent,
                (binding, agents, resources, None),
                String::new(),
                None,
                None,
            )
            .await
            .unwrap();
        let (sender, to) = if mail {
            (worker, "main".to_string())
        } else {
            (root, handle)
        };
        engine
            .mail_send(
                sender,
                MailEndpoint::Handle(to),
                MailKind::Message,
                "wake work".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        let runner = engine.clone();
        let stop =
            tokio::spawn(async move { runner.stop_for_restart(Duration::from_secs(5)).await });
        assert!(
            support::poll_until(Duration::from_secs(2), || async {
                engine.draining().is_some()
            })
            .await
        );
        release.notify_one();
        assert!(stop.await.unwrap().unwrap().stragglers.is_empty());
        supervisor
            .drain(FinishCause::Shutdown, Duration::from_secs(5))
            .await;
        assert_eq!(
            assistant_finishes(&engine.replay(root).await.unwrap(), root)[0].1,
            vec![(FinishReason::Cancelled, Some(FinishCause::Shutdown))]
        );
        let pending = engine.store().list_pending_resumes().await.unwrap();
        assert_eq!(
            (
                pending.len(),
                pending[0].session,
                provider.streams_for(root)
            ),
            (1, root, 0)
        );
    }
}

#[tokio::test]
async fn permission_pending_user_edit_snapshot_boundary() {
    for approve in [false, true] {
        let provider = Arc::new(ScriptProvider::default());
        let policy = hya_tool::InvocationPolicy::compile(
            hya_tool::PermissionModel::Default,
            vec![hya_tool::InvocationRule::new(
                hya_tool::PermissionTarget::Tool,
                "^write$",
                hya_tool::Mode::Ask,
            )],
        )
        .unwrap();
        let (permission, mut asks) =
            PermissionPlane::new_with_policy(PermissionRules::default(), policy);
        let (engine, mut agent) = scripted_engine_with_perm(
            provider.clone(),
            Arc::new(hya_tool_loader::builtins()),
            permission,
        )
        .await;
        let dir = support::TestDir::new("perm-edit-snap");
        agent.workdir = dir.path().into();
        tokio::fs::write(dir.path().join("partial.txt"), "before")
            .await
            .unwrap();
        let session = support::create_root_session(
            &engine,
            agent.name.as_str(),
            agent.model.as_str(),
            dir.path(),
        )
        .await;
        provider.script(session, Script::Write);
        engine
            .admit_user_prompt(session, "write".into())
            .await
            .unwrap();
        let turn = {
            let (e, a) = (engine.clone(), agent.clone());
            tokio::spawn(async move { e.run_turn(session, &a, CancellationToken::new()).await })
        };
        let ask = tokio::time::timeout(Duration::from_secs(5), asks.recv())
            .await
            .unwrap()
            .unwrap();
        let call = ask.call_id.unwrap();
        tokio::fs::write(dir.path().join("partial.txt"), "user edit")
            .await
            .unwrap();
        if approve {
            ask.reply.send(hya_tool::Decision::AllowOnce).unwrap();
        } else {
            assert!(engine.cancel_turn(session, FinishCause::UserCancel));
            drop(ask);
        }
        assert_eq!(
            turn.await.unwrap().unwrap(),
            if approve {
                FinishReason::Stop
            } else {
                FinishReason::Cancelled
            }
        );
        let events = engine.replay(session).await.unwrap();
        if approve {
            let before = events
                .iter()
                .find_map(|e| match &e.event {
                    Event::FilesChanged { files, .. } => Some(&files[0].before),
                    _ => None,
                })
                .unwrap();
            let hya_proto::FileState::Stored { hash, .. } = before else {
                panic!("{before:?}")
            };
            assert_eq!(
                engine
                    .store()
                    .file_blob(session, hash)
                    .await
                    .unwrap()
                    .unwrap(),
                b"user edit"
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e.event, Event::ToolResult { call: c, .. } if c == call))
            );
        } else {
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e.event, Event::FilesChanged { .. }))
            );
            assert_eq!(
                tokio::fs::read_to_string(dir.path().join("partial.txt"))
                    .await
                    .unwrap(),
                "user edit"
            );
        }
    }
}

#[tokio::test]
async fn sidecar_loss_flushes_resident_text_and_tool_changes() {
    for (hooks, writing) in [(false, false), (true, false), (false, true), (true, true)] {
        let provider = Arc::new(ScriptProvider::default());
        let tools = Arc::new(hya_tool_loader::builtins());
        tools.remove("write");
        let tool = Arc::new(WriteThenWait {
            written: Notify::new(),
        });
        tools.register(tool.clone()).unwrap();
        let (perm, _) = PermissionPlane::new(PermissionRules::default());
        let (engine, mut agent) = scripted_engine_with_perm(provider.clone(), tools, perm).await;
        let dir = support::TestDir::new("sidecar-flush");
        agent.workdir = dir.path().into();
        let root = make_session(&engine, None, "hya-main").await;
        let sidecar = Arc::new(LossSidecar {
            loss: CancellationToken::new(),
            terminated: Arc::new(Notify::new()),
            hooks,
        });
        let mut live = engine.bus().subscribe();
        let supervisor = ResidentSupervisor::start(engine.clone());
        let binding = engine.bind_runtime(&agent.workdir).unwrap();
        let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
        let (child, handle, _) = supervisor
            .spawn_resident_parked(
                root,
                agent.clone(),
                (binding, Arc::from([]), resources, Some(sidecar.clone())),
                "flush".into(),
                None,
                None,
            )
            .await
            .unwrap();
        provider.script(
            child,
            if writing {
                Script::Write
            } else {
                Script::PartialText
            },
        );
        engine
            .mail_send(
                root,
                MailEndpoint::parse(&handle),
                MailKind::Message,
                "work".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            if writing {
                tool.written.notified().await;
            } else {
                loop {
                    if matches!(live.recv().await.unwrap().event, Event::TextDelta { session, ref delta, .. } if session == child && delta == "partial answer") {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        sidecar.loss.cancel();
        tokio::time::timeout(Duration::from_secs(5), sidecar.terminated.notified())
            .await
            .unwrap();
        let events = engine.replay(child).await.unwrap();
        supervisor.end_member(root, &handle).await.unwrap();
        assert_eq!(
            assistant_finishes(&events, child)[0].1[0].0,
            FinishReason::Cancelled
        );
        if writing {
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e.event, Event::FilesChanged { .. }))
            );
        } else {
            for expected in ["partial answer", "thinking"] {
                assert!(events.iter().any(|e| matches!(&e.event, Event::TextReplace { text, .. } | Event::ReasoningReplace { text, .. } if text == expected)));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Resident recovery & superseded claim fencing
// ---------------------------------------------------------------------------

async fn recovery_engine(store: SessionStore) -> SessionEngine {
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(
        store,
        Arc::new(ProviderRouter::new()),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        permission,
        EventBus::default(),
    )
}

#[tokio::test]
async fn stale_claim_fences_tool_and_child_mutations() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let engine = recovery_engine(store.clone()).await;
    let root = SessionId::new();
    let old_binding = engine.runtime_registry().bind_turn(Path::new(".")).unwrap();
    let old_claim = store
        .try_claim_new(root, root, OwnerRunId::new())
        .await
        .unwrap();
    let recovered = store.recover_claim(root, owner).await.unwrap();

    for stale_event in [
        Event::ToolResult {
            session: root,
            message: MessageId::new(),
            part: PartId::new(),
            call: ToolCallId::new(),
            output: json!({"stale": true}),
            time_ms: 1,
        },
        Event::MemberFinished {
            session: root,
            member: MemberId::new(),
            status: MemberRunStatus::Done,
            summary: "late".into(),
            child: Some(SessionId::new()),
        },
    ] {
        assert!(matches!(
            engine.commit_resident_mutation(&old_claim, root, vec![stale_event]).await,
            Err(CoreError::Store(StoreError::StaleActorClaim { actor_id })) if actor_id == root
        ));
    }
    assert!(
        store.replay(root).await.unwrap().is_empty() && old_binding.resolve_tool("read").is_some()
    );
    engine
        .commit_resident_mutation(
            &recovered.claim,
            root,
            vec![Event::ToolResult {
                session: root,
                message: MessageId::new(),
                part: PartId::new(),
                call: ToolCallId::new(),
                output: json!({"current": true}),
                time_ms: 1,
            }],
        )
        .await
        .unwrap();
    assert_eq!(store.replay(root).await.unwrap().len(), 1);
}

#[tokio::test]
async fn queued_resident_resumes_running_aborts_and_repeat_recovery_is_idempotent() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let engine = recovery_engine(store.clone()).await;

    // 1. Queued mail resumes at inbox cursor 0.
    let (q_root, q_actor) = (SessionId::new(), SessionId::new());
    let _q_claim = store
        .try_claim_new(q_root, q_actor, OwnerRunId::new())
        .await
        .unwrap();
    for event in [
        Event::AgentRegistered {
            session: q_root,
            agent_session: q_actor,
            handle: "queued-1".into(),
            parent: Some("main".into()),
            agent_type: AgentName::new("queued"),
            mode: SubagentMode::Resident,
        },
        Event::ChannelCreated {
            session: q_root,
            channel: "DM-1".into(),
            kind: hya_proto::ChannelKind::Dm,
            unit: None,
            members: vec!["main".into(), "main/queued-1".into()],
        },
        Event::MailSent {
            session: q_root,
            from: "main".into(),
            to: MailEndpoint::Channel("DM-1".into()),
            kind: MailKind::Message,
            body: "queued".into(),
        },
    ] {
        store.append_event(q_root, &event).await.unwrap();
    }
    let q_rec = store.recover_claim(q_actor, owner).await.unwrap();
    assert_eq!(
        engine
            .recover_resident_actor(&q_rec, q_root, "main/queued-1")
            .await
            .unwrap()
            .work,
        ResidentRecovery::Queued { inbox_cursor: 0 }
    );

    // 2. Running turn aborts open tool + child + assistant, keeps queued-after flag, and repeat recovery is idempotent.
    let (r_root, r_actor) = (SessionId::new(), SessionId::new());
    let r_claim = store
        .try_claim_new(r_root, r_actor, OwnerRunId::new())
        .await
        .unwrap();
    let (msg, member) = (MessageId::new(), MemberId::new());
    for event in [
        Event::MessageStarted {
            session: r_actor,
            message: msg,
            role: Role::Assistant,
            agent: None,
            model: None,
        },
        Event::ToolCallRequested {
            session: r_actor,
            message: msg,
            part: PartId::new(),
            call: ToolCallId::new(),
            name: ToolName::new("read"),
            input: json!({"path": "README.md"}),
            provider_data: None,
        },
        Event::MemberSpawned {
            session: r_actor,
            member,
            child: Some(SessionId::new()),
            subagent_type: AgentName::new("child"),
            description: "running".into(),
            depth: 2,
            directive: String::new(),
            tool_call: None,
        },
        Event::MemberStatusChanged {
            session: r_actor,
            member,
            status: MemberRunStatus::Running,
        },
    ] {
        store.append_event(r_actor, &event).await.unwrap();
    }
    for event in [
        Event::AgentRegistered {
            session: r_root,
            agent_session: r_actor,
            handle: "running-1".into(),
            parent: Some("main".into()),
            agent_type: AgentName::new("running"),
            mode: SubagentMode::Resident,
        },
        Event::ChannelCreated {
            session: r_root,
            channel: "DM-1".into(),
            kind: hya_proto::ChannelKind::Dm,
            unit: None,
            members: vec!["main".into(), "main/running-1".into()],
        },
        Event::MailSent {
            session: r_root,
            from: "main".into(),
            to: MailEndpoint::Channel("DM-1".into()),
            kind: MailKind::Message,
            body: "running".into(),
        },
    ] {
        store.append_event(r_root, &event).await.unwrap();
    }
    engine
        .commit_resident_mutation(
            &r_claim,
            r_root,
            vec![
                Event::ResidentWorkStarted {
                    session: r_root,
                    actor_session: r_actor,
                    handle: "main/running-1".into(),
                    epoch: r_claim.epoch,
                    inbox_through: 1,
                },
                Event::AgentActivityChanged {
                    session: r_root,
                    handle: "main/running-1".into(),
                    status: RosterStatus::Busy,
                    current_task: Some("mail".into()),
                },
            ],
        )
        .await
        .unwrap();
    store
        .append_event(
            r_root,
            &Event::MailSent {
                session: r_root,
                from: "main".into(),
                to: MailEndpoint::Channel("DM-1".into()),
                kind: MailKind::Message,
                body: "after".into(),
            },
        )
        .await
        .unwrap();

    let r_rec = store.recover_claim(r_actor, owner).await.unwrap();
    assert_eq!(
        engine
            .recover_resident_actor(&r_rec, r_root, "main/running-1")
            .await
            .unwrap()
            .work,
        ResidentRecovery::AbortedRunning {
            inbox_cursor: 1,
            queued_after: true
        }
    );
    let proj_after = store.read_projection(r_root).await.unwrap();
    let entry = &proj_after.team.roster["main/running-1"];
    assert_eq!(
        (
            entry.status,
            entry.resident_cursor,
            entry.resident_work.is_none()
        ),
        (RosterStatus::Failed, 1, true)
    );
    let actor_proj = store.read_projection(r_actor).await.unwrap();
    assert_eq!(
        actor_proj.session.members[0].status,
        MemberRunStatus::Cancelled
    );
    assert_eq!(
        actor_proj.session.messages.last().unwrap().finish,
        Some(FinishReason::Cancelled)
    );

    // Repeating recovery emits no duplicate Failed status and keeps projection identical.
    let r_rec2 = store.recover_claim(r_actor, owner).await.unwrap();
    assert_eq!(
        engine
            .recover_resident_actor(&r_rec2, r_root, "main/running-1")
            .await
            .unwrap()
            .work,
        ResidentRecovery::Queued { inbox_cursor: 1 }
    );
    assert_eq!(store.read_projection(r_root).await.unwrap(), proj_after);
    assert_eq!(
        store
            .replay(r_root)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(
                e.event,
                Event::AgentActivityChanged {
                    status: RosterStatus::Failed,
                    ..
                }
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn resident_recovery_rolls_back_actor_and_root_failure_atomically() {
    let dir = support::TestDir::new("recovery-atomic");
    let db = dir
        .path()
        .join("sessions.db")
        .to_string_lossy()
        .into_owned();
    let store = SessionStore::connect(&db).await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let engine = recovery_engine(store.clone()).await;
    let (root, actor, handle) = (SessionId::new(), SessionId::new(), "main/recovery-atomic-1");
    let old_claim = store
        .try_claim_new(root, actor, OwnerRunId::new())
        .await
        .unwrap();
    for event in [
        Event::AgentRegistered {
            session: root,
            agent_session: actor,
            handle: "recovery-atomic-1".into(),
            parent: Some("main".into()),
            agent_type: AgentName::new("resident"),
            mode: SubagentMode::Resident,
        },
        Event::ChannelCreated {
            session: root,
            channel: "DM-1".into(),
            kind: hya_proto::ChannelKind::Dm,
            unit: None,
            members: vec!["main".into(), handle.into()],
        },
        Event::MailSent {
            session: root,
            from: "main".into(),
            to: MailEndpoint::Channel("DM-1".into()),
            kind: MailKind::Message,
            body: "recovery".into(),
        },
    ] {
        store.append_event(root, &event).await.unwrap();
    }
    store
        .append_event(
            actor,
            &Event::MessageStarted {
                session: actor,
                message: MessageId::new(),
                role: Role::Assistant,
                agent: None,
                model: None,
            },
        )
        .await
        .unwrap();
    engine
        .commit_resident_mutation(
            &old_claim,
            root,
            vec![
                Event::ResidentWorkStarted {
                    session: root,
                    actor_session: actor,
                    handle: handle.into(),
                    epoch: old_claim.epoch,
                    inbox_through: 1,
                },
                Event::AgentActivityChanged {
                    session: root,
                    handle: handle.into(),
                    status: RosterStatus::Busy,
                    current_task: Some("mail".into()),
                },
            ],
        )
        .await
        .unwrap();

    let recovered = store.recover_claim(actor, owner).await.unwrap();
    let (root_before, actor_before) = (
        store.replay(root).await.unwrap(),
        store.replay(actor).await.unwrap(),
    );
    let mut conn = SqliteConnection::connect(&format!("sqlite://{db}"))
        .await
        .unwrap();
    sqlx::query(&format!(
        "CREATE TRIGGER test_recovery_fail BEFORE INSERT ON event_log \
         WHEN instr(NEW.payload, '\"type\":\"agent_activity_changed\"') > 0 \
         AND instr(NEW.payload, '\"session\":\"{root}\"') > 0 \
         AND instr(NEW.payload, '\"current_task\":\"aborted by resident recovery\"') > 0 \
         BEGIN SELECT RAISE(ABORT, 'test recovery root failure'); END;"
    ))
    .execute(&mut conn)
    .await
    .unwrap();
    assert!(
        engine
            .recover_resident_actor(&recovered, root, handle)
            .await
            .is_err()
    );
    assert_eq!(store.replay(root).await.unwrap(), root_before);
    assert_eq!(store.replay(actor).await.unwrap(), actor_before);
    assert!(store.validate_actor_claim(&recovered.claim).await.is_ok());
}

#[tokio::test]
async fn superseded_stream_fences_text_reasoning_usage_and_tool_boundaries() {
    #[derive(Clone, Copy)]
    enum Boundary {
        Text,
        Reasoning,
        Usage,
        Tool,
    }

    struct Controlled {
        receiver: Mutex<Option<mpsc::UnboundedReceiver<Result<Event, ProviderError>>>>,
        entered: Notify,
    }
    #[async_trait]
    impl Provider for Controlled {
        fn id(&self) -> &str {
            "hya"
        }
        fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
            DevProvider::new().capabilities(model)
        }
        async fn stream(
            &self,
            _: CompletionRequest,
            _: SessionId,
            _: MessageId,
        ) -> Result<EventStream, ProviderError> {
            let rx = self.receiver.lock().unwrap().take().unwrap();
            self.entered.notify_one();
            Ok(Box::pin(stream::unfold(rx, |mut rx| async {
                rx.recv().await.map(|e| (e, rx))
            })))
        }
    }
    struct CountedTool(Arc<AtomicUsize>);
    #[async_trait]
    impl Tool for CountedTool {
        fn name(&self) -> &str {
            "bash"
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "bash".into(),
                description: "count".into(),
                input_schema: json!({"type":"object"}),
                output_schema: None,
            }
        }
        async fn execute(
            &self,
            _: &ToolCtx,
            _: serde_json::Value,
        ) -> Result<serde_json::Value, ToolError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        }
    }

    for boundary in [
        Boundary::Text,
        Boundary::Reasoning,
        Boundary::Usage,
        Boundary::Tool,
    ] {
        let dispatches = Arc::new(AtomicUsize::new(0));
        let tools = hya_tool_loader::builtins();
        tools.remove("bash");
        tools
            .register_with_permission(
                Arc::new(CountedTool(dispatches.clone())),
                hya_tool::ToolPermission::ReadOnly,
            )
            .unwrap();
        let dir = support::TestDir::new("superseded-boundary");
        let path = dir.path().join("sessions.db");
        let store = SessionStore::connect(path.to_str().unwrap()).await.unwrap();
        let other = SessionStore::connect(path.to_str().unwrap()).await.unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let provider = Arc::new(Controlled {
            receiver: Mutex::new(Some(rx)),
            entered: Notify::new(),
        });
        let (perm, _) = PermissionPlane::new(PermissionRules::default());
        let engine = Arc::new(SessionEngine::new(
            store.clone(),
            Arc::new(ProviderRouter::new().with(provider.clone())),
            support::test_runtime(Arc::new(tools)),
            perm,
            EventBus::default(),
        ));
        let agent = AgentSpec {
            name: "hya-task".into(),
            model: "hya/offline".into(),
            workdir: PathBuf::from("/tmp"),
            system_prompt: "t".into(),
            reasoning: None,
        };
        let root = make_session(&engine, None, "hya-main").await;
        let root_turn = engine.try_begin_turn(root).unwrap();
        let child = make_session(&engine, Some(root), "hya-task").await;
        let supervisor = ResidentSupervisor::start(engine.clone());
        let mut live = engine.bus().subscribe();
        supervisor
            .register_existing_resident(
                root,
                child,
                "worker-1".into(),
                agent,
                Some("stream".into()),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
            .await
            .unwrap();
        let message = engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .last()
            .unwrap()
            .id;
        let part = PartId::new();
        let reasoning = matches!(boundary, Boundary::Reasoning);
        tx.send(Ok(if reasoning {
            Event::ReasoningStart {
                session: child,
                message,
                part,
                reason: None,
            }
        } else {
            Event::TextStart {
                session: child,
                message,
                part,
            }
        }))
        .unwrap();
        tx.send(Ok(if reasoning {
            Event::ReasoningDelta {
                session: child,
                message,
                part,
                delta: "buffered".into(),
            }
        } else {
            Event::TextDelta {
                session: child,
                message,
                part,
                delta: "buffered".into(),
            }
        }))
        .unwrap();
        let complete_before = matches!(boundary, Boundary::Usage | Boundary::Tool);
        if complete_before {
            tx.send(Ok(Event::TextEnd {
                session: child,
                message,
                part,
            }))
            .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let ev = live.recv().await.unwrap().event;
                let ready = if complete_before {
                    matches!(ev, Event::TextEnd { session, .. } if session == child)
                } else if reasoning {
                    matches!(ev, Event::ReasoningDelta { session, delta, .. } if session == child && delta == "buffered")
                } else {
                    matches!(ev, Event::TextDelta { session, delta, .. } if session == child && delta == "buffered")
                };
                if ready { break; }
            }
        }).await.unwrap();
        let owner = OwnerRunId::new();
        other.claim_runtime_owner(owner).unwrap();
        other.recover_claim(child, owner).await.unwrap();
        let (before_child, before_root) = (
            store.replay(child).await.unwrap(),
            store.replay(root).await.unwrap(),
        );
        let _ = tx.send(Ok(match boundary {
            Boundary::Text => Event::TextEnd {
                session: child,
                message,
                part,
            },
            Boundary::Reasoning => Event::ReasoningEnd {
                session: child,
                message,
                part,
                provider_data: Some(json!({"opaque": true})),
            },
            Boundary::Usage => Event::MessageFinished {
                session: child,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                cause: None,
                tokens: Some(hya_proto::TokenUsage {
                    input: 11,
                    output: 7,
                    ..Default::default()
                }),
            },
            Boundary::Tool => Event::ToolCallRequested {
                session: child,
                message,
                part,
                call: ToolCallId::new(),
                name: "bash".into(),
                input: json!({"command": "count"}),
                provider_data: None,
            },
        }));
        drop(tx);
        assert!(
            support::poll_until(Duration::from_secs(2), || async {
                !engine.turn_active(child) && supervisor.episode_count() == 0
            })
            .await
        );
        let (after_child, after_root) = (
            store.replay(child).await.unwrap(),
            store.replay(root).await.unwrap(),
        );
        engine.begin_drain(FinishCause::Shutdown);
        drop(root_turn);
        supervisor
            .drain(FinishCause::Shutdown, Duration::from_secs(5))
            .await;
        assert_eq!(dispatches.load(Ordering::SeqCst), 0);
        assert_eq!((after_child, after_root), (before_child, before_root));
    }
}
