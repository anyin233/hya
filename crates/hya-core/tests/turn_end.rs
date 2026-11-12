//! End-event invariant: every assistant message ends with exactly one
//! `MessageFinished`, every non-terminal tool part reaches a terminal state,
//! and every member reaches a terminal status — on a graceful drain, a user
//! cancel, and a lead failure (which also broadcasts a wrap-up notice to the
//! team and never archives the lead).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt as _, stream};
use hya_core::{
    AgentSpec, BoundSidecarFactory, CoreError, CreateSession, EventBus, ResidentSupervisor,
    RuntimeCatalogRefresh, RuntimeRegistry, SessionEngine, SidecarEnvironment, SidecarHandle,
    SidecarStart, SubagentGovernor, SubagentLimits, TurnBinding,
};
use hya_proto::{
    AgentName, ArchiveReason, Envelope, Event, FinishCause, FinishReason, MailEndpoint, MailKind,
    MessageId, ModelRef, PartId, PartProjection, Role, RosterStatus, SessionId, ToolCallId,
    ToolName, ToolPartState, ToolSchema,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, Tool, ToolCtx, ToolError, ToolRegistry};
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// How a scripted session's next stream behaves.
#[derive(Clone, Copy)]
enum Script {
    /// Open a tool part, then stream nothing more until cancelled.
    Hang,
    /// Park until `release`, then answer with one text part.
    Gate,
    /// Fail the stream outright (a provider error).
    Fail,
    /// One complete tool-call round for the registered `gate_tool`, ending
    /// the round with the call open (the tool parks it).
    Tool,
}

/// Claims `hya/offline`. Sessions without a script answer immediately.
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
                    stream::iter(vec![Ok::<Event, ProviderError>(open)]).chain(stream::pending()),
                ))
            }
            Some(Script::Fail) => Err(ProviderError::Decode(
                "upstream exploded mid-turn".to_string(),
            )),
            Some(Script::Gate) => {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(text_answer(session, message))
            }
            Some(Script::Tool) => Ok(tool_call_stream(session, message)),
            None => Ok(text_answer(session, message)),
        }
    }
}

/// One complete tool-call round against the registered `gate_tool`: the round
/// ends with the call open (the tool parks on its release gate), so the test
/// controls exactly when the turn reaches its next round boundary.
fn tool_call_stream(session: SessionId, message: MessageId) -> EventStream {
    let events = FakeProvider::materialize(
        &[
            FakeStep::ToolCall {
                name: "gate_tool".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        session,
        message,
    );
    Box::pin(stream::iter(
        events.into_iter().map(Ok::<Event, ProviderError>),
    ))
}

/// A tool that parks until `release`, so a test can quiesce mid-tool and
/// decide when the turn's next round boundary arrives.
struct GatedTool {
    release: Notify,
}

impl GatedTool {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            release: Notify::new(),
        })
    }
}

#[async_trait]
impl Tool for GatedTool {
    fn name(&self) -> &str {
        "gate_tool"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("gate_tool"),
            description: "gated marker".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(
        &self,
        _ctx: &ToolCtx,
        _input: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        self.release.notified().await;
        Ok(json!({ "ok": true }))
    }
}

fn text_answer(session: SessionId, message: MessageId) -> EventStream {
    let events = FakeProvider::materialize(
        &[
            FakeStep::Text("ok".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        session,
        message,
    );
    Box::pin(stream::iter(
        events.into_iter().map(Ok::<Event, ProviderError>),
    ))
}

async fn engine_with_script() -> (Arc<SessionEngine>, AgentSpec, Arc<ScriptProvider>) {
    let provider = Arc::new(ScriptProvider::default());
    let (engine, agent) =
        scripted_engine(provider.clone(), Arc::new(ToolRegistry::builtins())).await;
    (engine, agent, provider)
}

/// A harness whose registry also carries [`GatedTool`], for tests that park a
/// real tool call mid-round and control when the next round boundary arrives.
async fn engine_with_gate_tool() -> (
    Arc<SessionEngine>,
    AgentSpec,
    Arc<ScriptProvider>,
    Arc<GatedTool>,
) {
    let provider = Arc::new(ScriptProvider::default());
    let gate = GatedTool::new();
    let tools = Arc::new(ToolRegistry::builtins());
    tools
        .register(gate.clone())
        .expect("register the gated tool");
    let (engine, agent) = scripted_engine(provider.clone(), tools).await;
    (engine, agent, provider, gate)
}

/// Build the scripted harness on `tools`, claiming the store's runtime owner
/// and wiring it into the engine (`with_runtime_owner`) exactly like the
/// production engine builder, so restart records are writable.
async fn scripted_engine(
    provider: Arc<ScriptProvider>,
    tools: Arc<ToolRegistry>,
) -> (Arc<SessionEngine>, AgentSpec) {
    let router = Arc::new(ProviderRouter::new().with(provider));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = hya_proto::OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let engine = Arc::new(
        SessionEngine::new(
            store,
            router,
            support::test_runtime(tools),
            perm,
            EventBus::default(),
        )
        .with_runtime_owner(owner)
        .with_governor(SubagentGovernor::new(SubagentLimits::default())),
    );
    let agent = AgentSpec {
        name: AgentName::new("hya-task"),
        model: ModelRef::new("hya/offline"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent)
}

fn next_engine(engine: &SessionEngine, provider: Arc<ScriptProvider>) -> SessionEngine {
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(
        engine.store().clone(),
        Arc::new(ProviderRouter::new().with(provider)),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    )
    .with_runtime_owner(engine.runtime_owner())
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

async fn stop_during_bind(
    engine: Arc<SessionEngine>,
    agent: AgentSpec,
    session: SessionId,
    gate: Arc<BindGate>,
) {
    let turn = {
        let engine = engine.clone();
        tokio::spawn(async move {
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    let stop = {
        let engine = engine.clone();
        tokio::spawn(async move { engine.stop_for_restart(Duration::from_secs(5)).await })
    };
    assert!(eventually(|| async { engine.draining().is_some() }).await);
    gate.release.notify_one();
    let report = stop.await.unwrap().unwrap();
    assert!(report.stragglers.is_empty(), "{report:?}");
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
        let engine = engine.clone();
        tokio::spawn(async move {
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await
        })
    };
    assert!(eventually(|| async { open_tool_parts(&engine, session).await == 1 }).await);
    engine
        .stop_for_restart(Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(turn.await.unwrap().unwrap(), FinishReason::Cancelled);
}

async fn restore_prompt_count(engine: &SessionEngine, session: SessionId) -> usize {
    engine.replay(session).await.unwrap().iter().filter(|envelope| matches!(
        &envelope.event, Event::TextDelta { delta, .. } if delta.contains("Your previous turn was interrupted by a restart.")
    )).count()
}

async fn pending_prompt(engine: &SessionEngine, session: SessionId) -> String {
    engine
        .store()
        .take_restart_resume(session, engine.runtime_owner())
        .await
        .unwrap()
        .unwrap()
        .into_iter()
        .find_map(|envelope| match envelope.event {
            Event::TextDelta { delta, .. } => Some(delta),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn restart_regression_stop_before_assistant_start_restores_the_turn() {
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    let gate = Arc::new(BindGate::default());
    let engine =
        Arc::new(next_engine(&engine, provider.clone()).with_catalog_refresh(gate.clone()));
    stop_during_bind(engine.clone(), agent.clone(), session, gate).await;
    assert_eq!(
        engine.store().list_pending_resumes().await.unwrap().len(),
        1
    );
    let next = next_engine(&engine, provider.clone());
    assert!(next.inject_restart_prompt(session).await.unwrap());
    assert_eq!(
        next.run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(restore_prompt_count(&next, session).await, 1);
    assert_eq!(provider.streams_for(session), 1);
}

#[tokio::test]
async fn restart_regression_second_stop_before_restored_assistant_start_is_a_new_interruption() {
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
    assert!(second.inject_restart_prompt(session).await.unwrap());
    assert_eq!(restore_prompt_count(&second, session).await, 1);
    stop_during_bind(second.clone(), agent.clone(), session, gate).await;
    assert_eq!(
        second.store().list_pending_resumes().await.unwrap().len(),
        1
    );
    let third = next_engine(&second, provider);
    assert!(third.inject_restart_prompt(session).await.unwrap());
    assert_eq!(
        third
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap(),
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
}

#[tokio::test]
async fn restart_crash_after_injection_starts_the_turn_without_another_prompt() {
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    stop_mid_turn(engine.clone(), agent.clone(), session, &provider).await;
    let second = next_engine(&engine, provider.clone());
    assert!(second.inject_restart_prompt(session).await.unwrap());
    assert_eq!(restore_prompt_count(&second, session).await, 1);
    // No shutdown finish is emitted: this process died before starting a turn.
    drop(second);
    let third = next_engine(&engine, provider.clone());
    assert!(third.inject_restart_prompt(session).await.unwrap());
    assert_eq!(restore_prompt_count(&third, session).await, 1);
    assert_eq!(
        third
            .run_turn(session, &agent, CancellationToken::new())
            .await
            .unwrap(),
        FinishReason::Stop
    );
    assert_eq!(provider.streams_for(session), 2);
    assert!(
        third
            .store()
            .list_pending_resumes()
            .await
            .unwrap()
            .is_empty()
    );
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

#[tokio::test]
async fn restart_during_sidecar_start_emits_a_shutdown_pair_and_restore() {
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    let sidecar = Arc::new(StartingSidecar::default());
    sidecar.release_cleanup.notify_one();
    let engine =
        Arc::new(next_engine(&engine, provider.clone()).with_sidecar_environment(sidecar.clone()));
    let turn = {
        let engine = engine.clone();
        tokio::spawn(async move {
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), sidecar.entered.notified())
        .await
        .unwrap();
    let report = engine
        .stop_for_restart(Duration::from_secs(5))
        .await
        .unwrap();
    assert!(report.stragglers.is_empty());
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

#[tokio::test]
async fn restart_deadline_closes_a_prestart_turn_while_sidecar_cleanup_is_blocked() {
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    let sidecar = Arc::new(StartingSidecar::default());
    let engine = Arc::new(next_engine(&engine, provider).with_sidecar_environment(sidecar.clone()));
    let turn = {
        let engine = engine.clone();
        tokio::spawn(async move {
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), sidecar.entered.notified())
        .await
        .unwrap();
    let report = engine.stop_for_restart(Duration::ZERO).await.unwrap();
    assert_eq!(report.stragglers, vec![session]);
    let pending = engine.store().list_pending_resumes().await.unwrap();
    assert_eq!(pending.len(), 1);
    sidecar.release_cleanup.notify_one();
    assert_eq!(turn.await.unwrap().unwrap(), FinishReason::Cancelled);
    assert_eq!(
        assistant_finishes(&engine.replay(session).await.unwrap(), session),
        vec![(
            pending[0].message,
            vec![(FinishReason::Cancelled, Some(FinishCause::Shutdown))]
        )]
    );
    assert_eq!(
        engine.store().list_pending_resumes().await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn restart_during_shell_binding_emits_a_shutdown_pair_and_restore() {
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    let gate = Arc::new(BindGate::default());
    let engine = Arc::new(next_engine(&engine, provider).with_catalog_refresh(gate.clone()));
    let turn = {
        let engine = engine.clone();
        tokio::spawn(async move {
            engine
                .run_shell(
                    session,
                    &agent,
                    "echo unused".into(),
                    CancellationToken::new(),
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    let report = engine
        .stop_for_restart(Duration::from_secs(5))
        .await
        .unwrap();
    assert!(report.stragglers.is_empty());
    assert_eq!(turn.await.unwrap().unwrap().1, FinishReason::Cancelled);
    let pending = engine.store().list_pending_resumes().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        assistant_finishes(&engine.replay(session).await.unwrap(), session),
        vec![(
            pending[0].message,
            vec![(FinishReason::Cancelled, Some(FinishCause::Shutdown))]
        )]
    );
}

#[tokio::test]
async fn user_cancel_during_runtime_binding_keeps_its_assistant_finish() {
    let (engine, agent, provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    let gate = Arc::new(BindGate::default());
    let engine = Arc::new(next_engine(&engine, provider).with_catalog_refresh(gate.clone()));
    let turn = {
        let engine = engine.clone();
        tokio::spawn(async move {
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    assert!(engine.cancel_turn(session, FinishCause::UserCancel));
    gate.release.notify_one();
    assert_eq!(turn.await.unwrap().unwrap(), FinishReason::Cancelled);
    let finishes = assistant_finishes(&engine.replay(session).await.unwrap(), session);
    assert_eq!(finishes.len(), 1);
    assert_eq!(
        finishes[0].1,
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
}

#[tokio::test]
async fn restart_while_waiting_to_begin_a_turn_still_records_the_interruption() {
    let (engine, agent, _) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    engine
        .admit_user_prompt(session, "work".into())
        .await
        .unwrap();
    let lease = engine.try_begin_turn(session).unwrap();
    let turn = engine.run_turn(session, &agent, CancellationToken::new());
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
async fn restart_regression_ordinary_turn_discards_an_uninjected_restore() {
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
    assert!(!next.inject_restart_prompt(session).await.unwrap());
    assert_eq!(restore_prompt_count(&next, session).await, 0);
}

async fn make_session(engine: &SessionEngine, parent: Option<SessionId>, agent: &str) -> SessionId {
    engine
        .create(CreateSession {
            parent,
            agent: AgentName::new(agent),
            model: ModelRef::new("hya/offline"),
            workdir: "/tmp".to_string(),
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

async fn eventually<F, Fut>(mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..500 {
        if check().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// One assistant message's finish events: `(finish, cause)` in log order.
type Finishes = Vec<(FinishReason, Option<FinishCause>)>;

/// Every assistant message of `session` with its finish events, in order.
fn assistant_finishes(events: &[Envelope], session: SessionId) -> Vec<(MessageId, Finishes)> {
    let mut order = Vec::new();
    let mut finishes: HashMap<MessageId, Vec<(FinishReason, Option<FinishCause>)>> = HashMap::new();
    for envelope in events {
        match &envelope.event {
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
        .map(|message| {
            let list = finishes.remove(&message).unwrap_or_default();
            (message, list)
        })
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
        .flat_map(|message| message.parts.iter())
        .filter(|part| {
            matches!(
                part,
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
    let team = engine.read_projection(root).await.unwrap().team;
    team.roster
        .values()
        .find(|entry| hya_proto::scope::leaf(&entry.handle) == leaf)
        .map(|entry| entry.status)
}

async fn harness_mail_to(engine: &SessionEngine, root: SessionId, leaf: &str) -> Vec<String> {
    let team = engine.read_projection(root).await.unwrap().team;
    team.inboxes
        .get(&format!("main/{leaf}"))
        .map(|inbox| {
            inbox
                .iter()
                .filter(|mail| mail.from == hya_proto::HARNESS_HANDLE)
                .map(|mail| mail.body.clone())
                .collect()
        })
        .unwrap_or_default()
}

async fn quiesced_notices(engine: &SessionEngine, root: SessionId) -> usize {
    engine
        .read_projection(root)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .filter(|message| {
            message.role == Role::System
                && message.parts.iter().any(|part| {
                    matches!(part, PartProjection::Text { text, .. } if text.contains("TEAM QUIESCED"))
                })
        })
        .count()
}

async fn archived(engine: &SessionEngine, root: SessionId) -> Vec<String> {
    engine
        .replay(root)
        .await
        .unwrap()
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::AgentArchived { handle, .. } => Some(handle.clone()),
            _ => None,
        })
        .collect()
}

/// A lead streaming a turn with two members streaming theirs: a graceful
/// drain closes all three turns — one `MessageFinished { cancelled, cause:
/// shutdown }` each, the open tool parts errored — and makes both members
/// terminal while the lead stays on the roster.
#[tokio::test]
async fn drain_closes_every_session_turn_with_a_cause() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    provider.script(root, Script::Hang);
    engine
        .admit_user_prompt(root, "lead the team".to_string())
        .await
        .unwrap();
    let lead = {
        let engine = engine.clone();
        let agent = agent.clone();
        tokio::spawn(async move {
            engine
                .run_turn(root, &agent, CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .expect("lead turn reaches the provider");

    register_main(&supervisor, &engine, &agent, root).await;
    let a = make_session(&engine, Some(root), "hya-task").await;
    let b = make_session(&engine, Some(root), "hya-task").await;
    for (session, handle) in [(a, "worker-a"), (b, "worker-b")] {
        provider.script(session, Script::Hang);
        supervisor
            .register_existing_resident(
                root,
                session,
                handle.to_string(),
                agent.clone(),
                Some("work".to_string()),
            )
            .await
            .unwrap();
    }
    assert!(
        eventually(|| async {
            engine.turn_active(a)
                && engine.turn_active(b)
                && open_tool_parts(&engine, a).await == 1
                && open_tool_parts(&engine, b).await == 1
                && open_tool_parts(&engine, root).await == 1
        })
        .await,
        "all three sessions are mid-turn with an open tool part"
    );

    let report = supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
    assert!(report.stragglers.is_empty(), "{report:?}");
    assert_eq!(
        report.cancelled.iter().copied().collect::<HashSet<_>>(),
        HashSet::from([root, a, b])
    );
    let finish = tokio::time::timeout(Duration::from_secs(5), lead)
        .await
        .expect("lead turn returns")
        .unwrap()
        .unwrap();
    assert_eq!(finish, FinishReason::Cancelled);

    for session in [root, a, b] {
        let events = engine.replay(session).await.unwrap();
        let finishes = assistant_finishes(&events, session);
        assert_eq!(finishes.len(), 1, "one assistant turn in {session}");
        assert_eq!(
            finishes[0].1,
            vec![(FinishReason::Cancelled, Some(FinishCause::Shutdown))],
            "exactly one finish with cause shutdown in {session}"
        );
        assert_eq!(open_tool_parts(&engine, session).await, 0);
        assert!(!engine.turn_active(session));
    }
    // A drain archives every member (reason `shutdown`) so a later run on
    // the same database can wake it with mail; the lead is never archived.
    for leaf in ["worker-a", "worker-b"] {
        assert_eq!(
            roster_status(&engine, root, leaf).await,
            None,
            "{leaf} left the live roster"
        );
    }
    let team = engine.read_projection(root).await.unwrap().team;
    for leaf in ["worker-a", "worker-b"] {
        let entry = team
            .archived
            .get(&format!("main/{leaf}"))
            .unwrap_or_else(|| panic!("{leaf} must be archived: {:?}", team.archived));
        assert_eq!(entry.reason, ArchiveReason::Shutdown);
    }
    assert_eq!(archived(&engine, root).await.len(), 2);
    assert!(
        roster_status(&engine, root, "main").await.is_some(),
        "the lead stays on the roster"
    );

    // Draining refuses new turns.
    engine
        .admit_user_prompt(root, "again".to_string())
        .await
        .unwrap();
    let refused = engine
        .run_turn(root, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(refused, FinishReason::Cancelled);
    assert_eq!(
        assistant_finishes(&engine.replay(root).await.unwrap(), root).len(),
        1,
        "no turn starts after the drain began"
    );
}

/// A user abort of the lead's turn records `cause: user_cancel` and sends no
/// leader-failed notice.
#[tokio::test]
async fn user_cancel_records_the_cause_and_does_not_broadcast() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let scout = make_session(&engine, Some(root), "hya-task").await;
    supervisor
        .register_existing_resident(root, scout, "scout".to_string(), agent.clone(), None)
        .await
        .unwrap();
    provider.script(root, Script::Hang);
    engine
        .admit_user_prompt(root, "lead".to_string())
        .await
        .unwrap();
    let lead = {
        let engine = engine.clone();
        let agent = agent.clone();
        tokio::spawn(async move {
            engine
                .run_turn(root, &agent, CancellationToken::new())
                .await
        })
    };
    assert!(
        eventually(|| async { open_tool_parts(&engine, root).await == 1 }).await,
        "lead is mid-turn"
    );
    assert!(engine.cancel_turn(root, FinishCause::UserCancel));
    let finish = tokio::time::timeout(Duration::from_secs(5), lead)
        .await
        .expect("lead turn returns")
        .unwrap()
        .unwrap();
    assert_eq!(finish, FinishReason::Cancelled);
    let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Cancelled, Some(FinishCause::UserCancel))]
    );
    assert_eq!(open_tool_parts(&engine, root).await, 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        harness_mail_to(&engine, root, "scout").await.is_empty(),
        "a user cancel is not a leader failure"
    );
    assert!(!engine.cancel_turn(root, FinishCause::UserCancel));
}

/// The lead's turn fails with a provider error while a member runs: the turn
/// ends `finish: error, cause: provider_error`, the member gets the harness
/// wrap-up mail, the lead is not archived, and no synthesis turn starts on
/// the dead lead even after the team goes quiet.
#[tokio::test]
async fn lead_failure_broadcasts_wrap_up_and_never_archives_the_lead() {
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
            "worker".to_string(),
            agent.clone(),
            Some("build the thing".to_string()),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .expect("member is streaming");

    provider.script(root, Script::Fail);
    engine
        .admit_user_prompt(root, "lead".to_string())
        .await
        .unwrap();
    let failed = engine
        .run_turn(root, &agent, CancellationToken::new())
        .await;
    assert!(failed.is_err(), "the lead's turn fails: {failed:?}");
    let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
    assert_eq!(finishes.len(), 1);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Error, Some(FinishCause::ProviderError))]
    );

    let notices = harness_mail_to(&engine, root, "worker").await;
    assert_eq!(notices.len(), 1, "exactly one wrap-up notice");
    assert!(notices[0].contains("LEADER FAILED"), "{}", notices[0]);

    // The member finishes its turn, is woken by the notice, and goes idle:
    // the team is quiet, but the dead lead gets no synthesis turn.
    provider.release.notify_one();
    assert!(
        eventually(|| async {
            provider.streams_for(worker) >= 2
                && roster_status(&engine, root, "worker").await == Some(RosterStatus::Idle)
        })
        .await,
        "the member handles the wrap-up notice and idles"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(quiesced_notices(&engine, root).await, 0);
    assert_eq!(
        provider.streams_for(root),
        1,
        "no harness-started lead turn"
    );
    assert!(
        archived(&engine, root).await.is_empty(),
        "the lead is never archived"
    );
    assert!(roster_status(&engine, root, "main").await.is_some());

    // The user resumes the lead: its session is live and runs normally.
    engine
        .admit_user_prompt(root, "resume".to_string())
        .await
        .unwrap();
    let resumed = engine
        .run_turn(root, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resumed, FinishReason::Stop);
}

/// Real-task run 1: a harness-started lead turn (a mail wake of `main`) fails
/// with a provider error. The lead used to be reported, handed off, and
/// archived like a failed member; now it stays live, the failure is the
/// turn's `finish: error`, and the team gets the wrap-up notice.
#[tokio::test]
async fn a_failed_lead_wake_does_not_archive_main() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "hya-task").await;
    supervisor
        .register_existing_resident(root, worker, "worker".to_string(), agent.clone(), None)
        .await
        .unwrap();
    provider.script(root, Script::Fail);
    engine
        .mail_send(
            worker,
            hya_proto::MailEndpoint::Handle("main".to_string()),
            hya_proto::MailKind::Message,
            "status: halfway".to_string(),
        )
        .await
        .unwrap();
    assert!(
        eventually(|| async {
            let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
            finishes.len() == 1 && !finishes[0].1.is_empty()
        })
        .await,
        "the woken lead turn runs and closes"
    );
    let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Error, Some(FinishCause::ProviderError))]
    );
    assert!(
        eventually(|| async { !harness_mail_to(&engine, root, "worker").await.is_empty() }).await,
        "the member is told to wrap up"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        archived(&engine, root).await.is_empty(),
        "main is never archived"
    );
    let reported_main = engine.replay(root).await.unwrap().iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::SubagentReported { handle, .. } | Event::HandoffCommitted { handle, .. }
                if handle == "main"
        )
    });
    assert!(!reported_main, "no report/handoff for the lead");
    assert!(roster_status(&engine, root, "main").await.is_some());
    assert_eq!(
        provider.streams_for(root),
        1,
        "no retry wake of the dead lead"
    );
}

/// `archive` on a member that is mid-turn cancels that turn — its message
/// closes with `finish: cancelled, cause: archived`, open tool parts error —
/// and then archives it (reason `archived_by_parent`). The lead's own turn is
/// untouched.
#[tokio::test]
async fn archive_cancels_a_busy_member_turn_with_cause_archived() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "hya-task").await;
    provider.script(worker, Script::Hang);
    supervisor
        .register_existing_resident(
            root,
            worker,
            "worker".to_string(),
            agent.clone(),
            Some("work".to_string()),
        )
        .await
        .unwrap();
    assert!(
        eventually(|| async {
            engine.turn_active(worker) && open_tool_parts(&engine, worker).await == 1
        })
        .await,
        "the member is mid-turn with an open tool part"
    );

    let receipt = supervisor
        .archive_member(root, "worker", "no longer needed")
        .await
        .unwrap();
    assert_eq!(receipt.handle, "main/worker");
    assert_eq!(receipt.session, worker);
    assert!(receipt.cancelled_turn, "{receipt:?}");

    assert!(!engine.turn_active(worker));
    let finishes = assistant_finishes(&engine.replay(worker).await.unwrap(), worker);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Cancelled, Some(FinishCause::Archived))],
        "the member's turn closes once with cause archived"
    );
    assert_eq!(open_tool_parts(&engine, worker).await, 0);
    let team = engine.read_projection(root).await.unwrap().team;
    assert!(!team.roster.contains_key("main/worker"));
    assert_eq!(
        team.archived.get("main/worker").map(|entry| entry.reason),
        Some(ArchiveReason::ArchivedByParent)
    );
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&worker),
        "the member's actor claim is released"
    );
}

/// A user archive from a client (`archive_subagent`, by child session) takes
/// the `archive` tool path: the busy member's turn closes with cause
/// archived and it leaves the live roster.
#[tokio::test]
async fn user_archive_of_a_busy_subagent_cancels_its_turn_and_archives_it() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "hya-task").await;
    provider.script(worker, Script::Hang);
    supervisor
        .register_existing_resident(
            root,
            worker,
            "worker".to_string(),
            agent.clone(),
            Some("work".to_string()),
        )
        .await
        .unwrap();
    assert!(
        eventually(|| async { engine.turn_active(worker) }).await,
        "the member is mid-turn"
    );

    let receipt = engine.archive_subagent(worker).await.unwrap();
    assert_eq!(receipt.handle, "main/worker");
    assert!(receipt.cancelled_turn, "{receipt:?}");
    assert!(!engine.turn_active(worker));
    let finishes = assistant_finishes(&engine.replay(worker).await.unwrap(), worker);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Cancelled, Some(FinishCause::Archived))]
    );
    let team = engine.read_projection(root).await.unwrap().team;
    assert!(!team.roster.contains_key("main/worker"));
    assert_eq!(
        team.archived.get("main/worker").map(|entry| entry.reason),
        Some(ArchiveReason::ArchivedByParent)
    );
    // A root session is not a subagent.
    let refused = engine.archive_subagent(root).await.unwrap_err().to_string();
    assert!(refused.contains("not a subagent"), "{refused}");
}

/// Interrupting a subagent (`CancelTurn` on its session) stops only its
/// turn: it stays on the roster, idle, and is not immediately re-run.
#[tokio::test]
async fn interrupting_a_subagent_cancels_only_its_turn() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "hya-task").await;
    provider.script(worker, Script::Hang);
    supervisor
        .register_existing_resident(
            root,
            worker,
            "worker".to_string(),
            agent.clone(),
            Some("work".to_string()),
        )
        .await
        .unwrap();
    assert!(
        eventually(|| async {
            engine.turn_active(worker) && open_tool_parts(&engine, worker).await == 1
        })
        .await,
        "the member is mid-turn with an open tool part"
    );

    assert!(engine.cancel_turn(worker, FinishCause::UserCancel));
    assert!(
        eventually(|| async { !engine.turn_active(worker) }).await,
        "the member's turn ends"
    );
    let finishes = assistant_finishes(&engine.replay(worker).await.unwrap(), worker);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Cancelled, Some(FinishCause::UserCancel))]
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        provider.streams_for(worker),
        1,
        "no follow-up turn after an interrupt"
    );
    assert!(!engine.turn_active(worker));
    let team = engine.read_projection(root).await.unwrap().team;
    assert!(
        team.roster.contains_key("main/worker"),
        "an interrupt does not archive"
    );
}

/// Register a resident `handle` under `root` that parks mid-turn, and wait
/// until it has an open tool part.
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
            handle.to_string(),
            agent.clone(),
            Some("work".to_string()),
        )
        .await
        .unwrap();
    assert!(
        eventually(|| async {
            engine.turn_active(member) && open_tool_parts(engine, member).await == 1
        })
        .await,
        "`{handle}` is mid-turn with an open tool part"
    );
    member
}

/// Cancelling the lead (`cancel_user_turn` on the root) stops every
/// subagent: their turns are cancelled, nothing restarts them — no mail
/// wake, no `TEAM QUIESCED` synthesis — and the lead's mail to them is
/// refused, until the user acts on the team again.
#[tokio::test]
async fn cancelling_the_lead_stops_every_subagent_until_the_user_acts() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let first = hanging_member(&engine, &supervisor, &provider, &agent, root, "first").await;
    let second = hanging_member(&engine, &supervisor, &provider, &agent, root, "second").await;

    assert!(engine.cancel_user_turn(root), "the subagents' turns cancel");
    assert!(
        eventually(|| async { !engine.turn_active(first) && !engine.turn_active(second) }).await,
        "both subagent turns end"
    );
    for member in [first, second] {
        let finishes = assistant_finishes(&engine.replay(member).await.unwrap(), member);
        assert_eq!(
            finishes[0].1,
            vec![(FinishReason::Cancelled, Some(FinishCause::UserCancel))]
        );
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(provider.streams_for(first), 1, "no follow-up turn");
    assert_eq!(provider.streams_for(second), 1, "no follow-up turn");
    assert_eq!(
        quiesced_notices(&engine, root).await,
        0,
        "the idle team does not wake the lead"
    );
    assert_eq!(provider.streams_for(root), 0, "the lead stays stopped");
    let team = engine.read_projection(root).await.unwrap().team;
    assert!(team.roster.contains_key("main/first") && team.roster.contains_key("main/second"));

    let refused = engine
        .mail_send(
            root,
            MailEndpoint::Handle("main/first".to_string()),
            MailKind::Message,
            "resume your work".to_string(),
        )
        .await
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("the user stopped `main/first`"),
        "{refused}"
    );

    // The user prompts the lead: the stop lifts, and its mail wakes again.
    engine.resume_after_user_stop(root);
    engine
        .mail_send(
            root,
            MailEndpoint::Handle("main/first".to_string()),
            MailKind::Message,
            "resume your work".to_string(),
        )
        .await
        .unwrap();
    assert!(
        eventually(|| async { provider.streams_for(first) == 2 }).await,
        "the lead's mail wakes the subagent again"
    );
    assert_eq!(provider.streams_for(second), 1, "unmailed, it stays idle");
}

/// Interrupting one subagent stops it alone: its sibling keeps running, the
/// lead's mail to it is refused, and the user's steer wakes it.
#[tokio::test]
async fn interrupting_one_subagent_stops_it_until_the_user_steers_it() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let first = hanging_member(&engine, &supervisor, &provider, &agent, root, "first").await;
    let second = hanging_member(&engine, &supervisor, &provider, &agent, root, "second").await;

    assert!(engine.cancel_user_turn(first));
    assert!(eventually(|| async { !engine.turn_active(first) }).await);
    assert!(engine.turn_active(second), "the sibling keeps running");
    assert!(
        engine
            .mail_send(
                root,
                MailEndpoint::Handle("main/first".to_string()),
                MailKind::Message,
                "keep going".to_string(),
            )
            .await
            .is_err(),
        "the lead cannot restart it"
    );

    engine.steer_subagent(first, "carry on").await.unwrap();
    assert!(
        eventually(|| async { provider.streams_for(first) == 2 }).await,
        "the user's steer wakes it"
    );
    assert!(engine.cancel_turn(second, FinishCause::UserCancel));
}

/// Interrupting a subagent that is idle — no turn running, none owed — stops
/// nothing: `CancelTurn` reports nothing cancelled, and the lead's mail
/// still reaches and wakes it.
#[tokio::test]
async fn interrupting_an_idle_subagent_stops_nothing() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let member = make_session(&engine, Some(root), "hya-task").await;
    supervisor
        .register_existing_resident(
            root,
            member,
            "idle".to_string(),
            agent.clone(),
            Some("work".to_string()),
        )
        .await
        .unwrap();
    assert!(
        eventually(|| async { provider.streams_for(member) == 1 && !engine.turn_active(member) })
            .await,
        "its first turn ran and ended"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(!engine.cancel_user_turn(member), "nothing was running");
    assert!(!engine.stopped_by_user(member), "nothing was stopped");
    engine
        .mail_send(
            root,
            MailEndpoint::Handle("main/idle".to_string()),
            MailKind::Message,
            "next task".to_string(),
        )
        .await
        .unwrap();
    assert!(
        eventually(|| async { provider.streams_for(member) == 2 }).await,
        "the lead's mail wakes it"
    );
}

#[tokio::test]
async fn stop_cancels_root_and_child_turns_but_only_queues_the_root() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let child = make_session(&engine, Some(root), "hya-task").await;
    engine
        .store()
        .append_event(
            root,
            &Event::AgentRegistered {
                session: root,
                agent_session: child,
                handle: "worker".into(),
                parent: Some("main".into()),
                agent_type: AgentName::new("hya-task"),
                mode: hya_proto::SubagentMode::Resident,
            },
        )
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for session in [root, child] {
        provider.script(session, Script::Hang);
        engine
            .admit_user_prompt(session, "continue working".into())
            .await
            .unwrap();
        let engine = engine.clone();
        let agent = agent.clone();
        tasks.push(tokio::spawn(async move {
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await
        }));
    }
    assert!(
        eventually(|| async {
            open_tool_parts(&engine, root).await == 1 && open_tool_parts(&engine, child).await == 1
        })
        .await
    );
    let report = engine
        .stop_for_restart(Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(report.cancelled.len(), 2);
    assert!(report.stragglers.is_empty());
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap(), FinishReason::Cancelled);
    }
    let pending = engine.store().list_pending_resumes().await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].session, root);
    engine
        .store()
        .append_event(
            root,
            &Event::AgentArchived {
                session: root,
                handle: "main/worker".into(),
                child,
                reason: ArchiveReason::Shutdown,
            },
        )
        .await
        .unwrap();
    let prompt = pending_prompt(&engine, root).await;
    assert!(prompt.contains("bash (call "));
    assert!(prompt.contains("Interrupted subagents: main/worker."));

    assert_eq!(provider.streams_for(root), 1);
    assert_eq!(provider.streams_for(child), 1);
}

#[tokio::test]
async fn stop_interrupts_a_running_tool_without_recording_success() {
    let (engine, agent, provider, _tool) = engine_with_gate_tool().await;
    let session = make_session(&engine, None, "hya-main").await;
    provider.script(session, Script::Tool);
    engine
        .admit_user_prompt(session, "run tool".into())
        .await
        .unwrap();
    let task = {
        let engine = engine.clone();
        tokio::spawn(async move {
            engine
                .run_turn(session, &agent, CancellationToken::new())
                .await
        })
    };
    assert!(eventually(|| async { open_tool_parts(&engine, session).await == 1 }).await);
    engine
        .stop_for_restart(Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap(), FinishReason::Cancelled);
    let rows = engine.store().list_pending_resumes().await.unwrap();
    assert_eq!(rows.len(), 1);
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
}

#[tokio::test]
async fn stop_does_not_restore_a_finished_turn_still_holding_its_lease() {
    let (engine, _agent, _provider) = engine_with_script().await;
    let session = make_session(&engine, None, "hya-main").await;
    let lease = engine.try_begin_turn(session).unwrap();
    let message = MessageId::new();
    for event in [
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
            agent: None,
            model: None,
        },
        Event::MessageFinished {
            session,
            message,
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

#[tokio::test]
async fn stop_restores_a_shell_call_after_its_shutdown_io_error() {
    let shell = Arc::new(ShutdownErrorShell {
        entered: Notify::new(),
    });
    let tools = Arc::new(ToolRegistry::from_tool_families(&["hya/extended-tools"]));
    tools.register(shell.clone()).unwrap();
    let (engine, agent) = scripted_engine(Arc::new(ScriptProvider::default()), tools).await;
    let session = make_session(&engine, None, "hya-main").await;
    let mut task = {
        let engine = engine.clone();
        tokio::spawn(async move {
            engine
                .run_shell(session, &agent, "waiting".into(), CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            () = shell.entered.notified() => {},
            result = &mut task => panic!("shell ended before executing: {result:?}"),
        }
    })
    .await
    .unwrap();
    engine
        .stop_for_restart(Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap().1, FinishReason::Cancelled);
    let rows = engine.store().list_pending_resumes().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        pending_prompt(&engine, session)
            .await
            .contains("bash (call ")
    );
    assert!(
        !engine
            .replay(session)
            .await
            .unwrap()
            .iter()
            .any(|entry| matches!(entry.event, Event::ToolResult { .. }))
    );
}
