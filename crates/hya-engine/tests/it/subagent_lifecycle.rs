//! Consolidated integration contracts for subagent/resident lifecycle,
//! governor limits, activation sidecars, handle naming, coordination tools,
//! and delta-path SQL isolation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use hya_bundle::{
    AgentRole, Bundle, BundleCatalog, BundleIdentity, BundleSource, HandoffPolicy, ModelPolicy,
    PreparedAgent, PreparedResource, ResourceView, SourceFile, prepare_package,
};
use hya_engine::handle_naming::{HandleRng, SplitMix64, operator_names};
use hya_engine::hooks::{
    NativeChatParamsInput, NativeCommandBeforeInput, NativeMessageUserBeforeInput,
    NativeTextCompleteInput, NativeToolAfterInput, NativeToolAfterOutcome, NativeToolBeforeInput,
    NativeToolBeforeOutcome,
};
use hya_engine::{
    AgentCatalog, AgentSpec, BoundSidecarFactory, BoundSpawnSender, CoreError, CreateSession,
    EventBus, NativeHook, ResidentRecovery, ResidentSupervisor, RuntimeSource, RuntimeSourceId,
    SessionEngine, SessionStoreExt as _, SidecarEnvironment, SidecarHandle, SidecarLifecycle,
    SidecarStart, SubagentGovernor, SubagentLimits, SummarizeOptions, Summarizer, TaskSpawnOrigin,
    TerminalHandoffOptions, TurnBinding, run_lifecycle_service,
};
use hya_proto::{
    ActorEpoch, AgentName, ChannelKind, Envelope, Event, FinishCause, FinishReason, MailEndpoint,
    MailKind, MemberRunStatus, Message, MessageId, ModelRef, Part, PartId, PartProjection, Role,
    RosterStatus, SessionId, SessionKind, SubagentMode, ToolCallId, ToolName, ToolSchema, scope,
};
use hya_provider::{
    Capabilities, CompletionRequest, DevProvider, EventStream, FakeProvider, FakeStep, Provider,
    ProviderError, ProviderRouter,
};
use hya_store::{SessionStore, bench};
use hya_tool::extension::{
    ExtensionEvent, handoff_projection, members_projection, projection_reducers, team_projection,
};
use hya_tool::{
    Action, LifecyclePlane, MemberOutcome, Mode, PermissionPlane, PermissionRules, ResolvedTool,
    Resource, Rule, Tool, ToolCtx, ToolError, ToolPermission,
};
use hya_tool::{ArchiveReason, ReportOutcome};
use serde_json::{Value, json};
use sqlx::{Connection, SqliteConnection};
use tokio::sync::{Barrier, Notify, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::support;
use support::resident::{ResidentTurn, run_residents};

const SIDECAR_TOOL: &str = "bundle:hya/sidecar-permission/tool/echo";
const END_BOUND: Duration = Duration::from_secs(3);

fn empty_workdir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let p = std::env::temp_dir().join(format!("hya-it-subagent-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&p);
        p
    })
}

fn fake_caps(model: &ModelRef, max_context: u32) -> Option<Capabilities> {
    (model.as_str() == "fake").then_some(Capabilities {
        streaming_tool_calls: true,
        usage_reporting: true,
        max_context,
        ..Capabilities::default()
    })
}

fn stream_steps(steps: &[FakeStep], session: SessionId, message: MessageId) -> EventStream {
    Box::pin(stream::iter(
        FakeProvider::materialize(steps, session, message)
            .into_iter()
            .map(Ok::<Event, ProviderError>),
    ))
}

struct StaticFake(&'static str);
#[async_trait]
impl Provider for StaticFake {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m, 200_000)
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        Ok(stream_steps(
            &[
                FakeStep::Text(self.0.into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            s,
            m,
        ))
    }
}

struct AckProbeProvider {
    polls: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for AckProbeProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m, 200_000)
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        Ok(stream_steps(
            &[
                FakeStep::Text("ACKTEXT".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            s,
            m,
        ))
    }
}

struct BarrierProvider {
    polls: Arc<AtomicUsize>,
    entered: mpsc::UnboundedSender<SessionId>,
    release: Arc<Notify>,
}
#[async_trait]
impl Provider for BarrierProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m, 200_000)
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.entered
            .send(s)
            .map_err(|_| ProviderError::Decode("barrier dropped".into()))?;
        self.release.notified().await;
        Ok(stream_steps(
            &[
                FakeStep::Text("RESIDENT_STOP_TEXT".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            s,
            m,
        ))
    }
}

struct PollGateProvider {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
#[async_trait]
impl Provider for PollGateProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m, 200_000)
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let (entered, release) = (self.entered.clone(), self.release.clone());
        Ok(Box::pin(stream::once(async move {
            entered.notify_one();
            release.notified().await;
            Ok(Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            })
        })))
    }
}

struct ConcurrencyProbe {
    current: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}
#[async_trait]
impl Provider for ConcurrencyProbe {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m, 200_000)
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(10)).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        Ok(stream_steps(
            &[
                FakeStep::Text("MEMBERTEXT".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            s,
            m,
        ))
    }
}

struct SidecarEchoProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    turn: AtomicUsize,
}
#[async_trait]
impl Provider for SidecarEchoProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m, 200_000)
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        let steps = if self.turn.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                FakeStep::ToolCall {
                    name: "echo".into(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        } else {
            vec![FakeStep::Finish(FinishReason::Stop)]
        };
        Ok(stream_steps(&steps, s, m))
    }
}

struct SidecarProbeTool {
    name: String,
    calls: Arc<AtomicUsize>,
    inputs: Arc<Mutex<Vec<Value>>>,
}
#[async_trait]
impl Tool for SidecarProbeTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name.clone()),
            description: "sidecar probe".into(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
        }
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        ctx.permission
            .assert(Action::Tool, Resource::Tool(self.name.clone()))
            .await?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inputs.lock().unwrap().push(input);
        Ok(json!({"ok": true}))
    }
}

fn sidecar_bundle() -> Bundle {
    Bundle {
        check: None,
        format_version: 4,
        identity: BundleIdentity {
            id: "hya/sidecar-permission".into(),
            version: "0.0.0".into(),
            publisher: "hya-tests".into(),
        },
        namespace: None,
        digest: "test-only".into(),
        agents: vec![PreparedAgent {
            private: false,
            id: AgentName::new("sidecar-agent"),
            description: None,
            role: AgentRole::Subagent,
            color: None,
            prompt: Some("sidecar permission prompt".into()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            resource_view: ResourceView::default(),
            can_spawn: vec![],
            hook_refs: vec![],
            advertise: vec![],
            handoff: HandoffPolicy::Model,
        }],
        tools: vec![PreparedResource {
            local_id: "echo".into(),
            stable_id: SIDECAR_TOOL.into(),
            source_path: "tools/echo.js".into(),
            digest: "test-only-tool".into(),
            content: "export default {}".into(),
            binary_base64: None,
            aliases: vec![],
            overrides: None,
        }],
        skills: vec![],
        mcp: vec![],
        hooks: vec![],
        extensions: vec![],
        tui: None,
        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        commands: Vec::new(),
        runtime: None,
        permission_modes: vec![],
        cli: Vec::new(),
        requires: Vec::new(),
        default_agent: None,
        overrides: None,
    }
}

struct AckGateFactory {
    starts: mpsc::UnboundedSender<SidecarStart>,
    ready: Arc<Notify>,
}
struct AckGateHandle(Arc<Notify>);
#[async_trait]
impl BoundSidecarFactory for AckGateFactory {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        self.starts
            .send(start)
            .map_err(|_| CoreError::Invalid("start rx dropped".into()))?;
        Ok(Box::new(AckGateHandle(self.ready.clone())))
    }
}
#[async_trait]
impl SidecarHandle for AckGateHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.0.notified().await;
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
}

#[derive(Clone)]
struct SidecarCfg {
    bindings: Arc<[ResolvedTool]>,
    hooks: Option<Arc<dyn NativeHook>>,
    starts: Arc<AtomicUsize>,
    ready_calls: Arc<AtomicUsize>,
    healthy: Arc<AtomicBool>,
    shutdowns: Arc<AtomicUsize>,
    terminates: Arc<AtomicUsize>,
    terminated: Option<Arc<Notify>>,
    loss: Option<CancellationToken>,
    loss_first_only: bool,
    fail_ready: bool,
    fail_terminate: bool,
    drift_after_first: bool,
}
impl Default for SidecarCfg {
    fn default() -> Self {
        Self {
            bindings: Arc::from([]),
            hooks: None,
            starts: Arc::new(AtomicUsize::new(0)),
            ready_calls: Arc::new(AtomicUsize::new(0)),
            healthy: Arc::new(AtomicBool::new(true)),
            shutdowns: Arc::new(AtomicUsize::new(0)),
            terminates: Arc::new(AtomicUsize::new(0)),
            terminated: None,
            loss: None,
            loss_first_only: false,
            fail_ready: false,
            fail_terminate: false,
            drift_after_first: false,
        }
    }
}
/// Revival resolves its sidecar from the engine environment, as production does.
impl SidecarEnvironment for SidecarCfg {
    fn factory_for(
        &self,
        _: &TurnBinding,
        _: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        Ok(Some(Arc::new(self.clone())))
    }
}
struct CfgHandle {
    cfg: SidecarCfg,
    loss: Option<CancellationToken>,
}
#[async_trait]
impl BoundSidecarFactory for SidecarCfg {
    async fn start(&self, _: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        let attempt = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        if self.drift_after_first && attempt != 1 {
            return Err(CoreError::Invalid(
                "bundle sidecar declaration drift".into(),
            ));
        }
        let loss = if self.loss_first_only {
            (attempt == 1).then(|| self.loss.clone()).flatten()
        } else {
            self.loss.clone()
        };
        Ok(Box::new(CfgHandle {
            cfg: self.clone(),
            loss,
        }))
    }
}
#[async_trait]
impl SidecarHandle for CfgHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.cfg.ready_calls.fetch_add(1, Ordering::SeqCst);
        if self.cfg.fail_ready {
            return Err(CoreError::Invalid("sidecar ACK rejected".into()));
        }
        self.cfg.healthy.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.cfg.shutdowns.fetch_add(1, Ordering::SeqCst);
        self.cfg.healthy.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn is_healthy(&self) -> bool {
        self.cfg.healthy.load(Ordering::SeqCst)
    }
    fn loss_token(&self) -> Option<CancellationToken> {
        self.loss.clone()
    }
    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.cfg.terminates.fetch_add(1, Ordering::SeqCst);
        self.cfg.healthy.store(false, Ordering::SeqCst);
        if let Some(t) = &self.cfg.terminated {
            t.notify_one();
        }
        if self.cfg.fail_terminate {
            return Err(CoreError::Invalid("test resident terminate failure".into()));
        }
        Ok(())
    }
    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        self.cfg.bindings.clone()
    }
    fn hook_dispatcher(&self) -> Option<hya_engine::HookEntry> {
        self.cfg.hooks.clone().map(hya_engine::HookEntry::new)
    }
}

#[derive(Clone, Default)]
struct ActivationHookProbe {
    tool_before: Arc<AtomicUsize>,
    tool_after: Arc<AtomicUsize>,
    leak: Arc<AtomicUsize>,
    events: Arc<Mutex<Vec<Envelope>>>,
}
#[async_trait]
impl NativeHook for ActivationHookProbe {
    fn id(&self) -> &str {
        "ActivationHookProbe"
    }
    fn hook_points(&self) -> &[hya_runtime_abi::HookPoint] {
        &[
            hya_runtime_abi::HookPoint::EventObserve,
            hya_runtime_abi::HookPoint::CommandExecuteBefore,
            hya_runtime_abi::HookPoint::TextComplete,
            hya_runtime_abi::HookPoint::MessageUserBefore,
            hya_runtime_abi::HookPoint::ChatParams,
            hya_runtime_abi::HookPoint::ToolExecuteBefore,
            hya_runtime_abi::HookPoint::ToolExecuteAfter,
        ]
    }

    fn on_event(&self, e: &Envelope) {
        self.events.lock().unwrap().push(e.clone());
    }
    async fn command_execute_before(
        &self,
        i: NativeCommandBeforeInput,
    ) -> Result<String, hya_runtime_abi::HostError> {
        self.leak.fetch_add(1, Ordering::SeqCst);
        Ok(i.text)
    }
    async fn text_complete(
        &self,
        i: NativeTextCompleteInput,
    ) -> Result<String, hya_runtime_abi::HostError> {
        self.leak.fetch_add(1, Ordering::SeqCst);
        Ok(i.text)
    }
    async fn message_user_before(
        &self,
        i: NativeMessageUserBeforeInput,
    ) -> Result<String, hya_runtime_abi::HostError> {
        self.leak.fetch_add(1, Ordering::SeqCst);
        Ok(i.text)
    }
    async fn chat_params(
        &self,
        i: NativeChatParamsInput,
    ) -> Result<serde_json::Value, hya_runtime_abi::HostError> {
        self.leak.fetch_add(1, Ordering::SeqCst);
        Ok(i.params)
    }
    async fn tool_execute_before(
        &self,
        i: NativeToolBeforeInput,
    ) -> Result<NativeToolBeforeOutcome, hya_runtime_abi::HostError> {
        self.tool_before.fetch_add(1, Ordering::SeqCst);
        let mut input = i.input;
        input["hooked"] = json!(true);
        Ok(NativeToolBeforeOutcome::Continue { input })
    }
    async fn tool_execute_after(
        &self,
        i: NativeToolAfterInput,
    ) -> Result<NativeToolAfterOutcome, hya_runtime_abi::HostError> {
        self.tool_after.fetch_add(1, Ordering::SeqCst);
        Ok(NativeToolAfterOutcome::Continue { result: i.result })
    }
}

#[derive(Clone, Default)]
struct PluginHookProbe {
    chat_params: Arc<AtomicUsize>,
    tool_before: Arc<AtomicUsize>,
}
#[async_trait]
impl NativeHook for PluginHookProbe {
    fn id(&self) -> &str {
        "PluginHookProbe"
    }
    fn hook_points(&self) -> &[hya_runtime_abi::HookPoint] {
        &[
            hya_runtime_abi::HookPoint::ChatParams,
            hya_runtime_abi::HookPoint::ToolExecuteBefore,
        ]
    }

    async fn chat_params(
        &self,
        i: NativeChatParamsInput,
    ) -> Result<serde_json::Value, hya_runtime_abi::HostError> {
        self.chat_params.fetch_add(1, Ordering::SeqCst);
        Ok(i.params)
    }
    async fn tool_execute_before(
        &self,
        i: NativeToolBeforeInput,
    ) -> Result<NativeToolBeforeOutcome, hya_runtime_abi::HostError> {
        self.tool_before.fetch_add(1, Ordering::SeqCst);
        Ok(NativeToolBeforeOutcome::Continue { input: i.input })
    }
}

#[derive(Clone, Copy)]
enum HookLossStage {
    Before,
    After,
    Event,
}
struct HookLossDispatcher {
    stage: HookLossStage,
    healthy: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl NativeHook for HookLossDispatcher {
    fn id(&self) -> &str {
        "HookLossDispatcher"
    }
    fn hook_points(&self) -> &[hya_runtime_abi::HookPoint] {
        &[
            hya_runtime_abi::HookPoint::EventObserve,
            hya_runtime_abi::HookPoint::ToolExecuteBefore,
            hya_runtime_abi::HookPoint::ToolExecuteAfter,
        ]
    }

    fn on_event(&self, _: &Envelope) {
        if matches!(self.stage, HookLossStage::Event)
            && self
                .healthy
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::SeqCst)
    }
    async fn tool_execute_before(
        &self,
        i: NativeToolBeforeInput,
    ) -> Result<NativeToolBeforeOutcome, hya_runtime_abi::HostError> {
        Ok(match self.stage {
            HookLossStage::Before => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.healthy.store(false, Ordering::SeqCst);
                NativeToolBeforeOutcome::Veto {
                    reason: "hook transport lost".into(),
                }
            }
            HookLossStage::After => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                NativeToolBeforeOutcome::Continue { input: i.input }
            }
            HookLossStage::Event => NativeToolBeforeOutcome::Continue { input: i.input },
        })
    }
    async fn tool_execute_after(
        &self,
        i: NativeToolAfterInput,
    ) -> Result<NativeToolAfterOutcome, hya_runtime_abi::HostError> {
        if matches!(self.stage, HookLossStage::After) {
            self.healthy.store(false, Ordering::SeqCst);
        }
        Ok(NativeToolAfterOutcome::Continue { result: i.result })
    }
}

// ---- Shared lifecycle helpers ----

async fn governed_engine(
    limits: SubagentLimits,
    provider: Arc<dyn Provider>,
) -> (Arc<SessionEngine>, AgentSpec) {
    let engine = Arc::new(
        support::engine_with_provider(provider, PermissionRules::default())
            .await
            .with_governor(SubagentGovernor::new(limits)),
    );
    (
        engine,
        support::agent_spec("hya-main", "fake", empty_workdir()),
    )
}

async fn dev_engine(limits: SubagentLimits) -> (Arc<SessionEngine>, AgentSpec) {
    let engine = Arc::new(
        support::engine_with_provider(Arc::new(DevProvider::new()), PermissionRules::default())
            .await
            .with_governor(SubagentGovernor::new(limits)),
    );
    (
        engine,
        support::agent_spec("hya-task", "hya/offline", empty_workdir()),
    )
}

async fn create_child(
    engine: &SessionEngine,
    parent: SessionId,
    agent: &str,
    model: &str,
) -> SessionId {
    engine
        .create(CreateSession {
            parent: Some(parent),
            agent: AgentName::new(agent),
            model: ModelRef::new(model),
            workdir: empty_workdir().to_string_lossy().into_owned(),
            project: None,
            kind: SessionKind::Project,
        })
        .await
        .unwrap()
}

async fn send_mail(engine: &SessionEngine, from: SessionId, to: &str, body: &str) {
    engine
        .mail_send(
            from,
            MailEndpoint::Handle(to.into()),
            MailKind::Message,
            body.into(),
        )
        .await
        .unwrap();
}

async fn spawn_res(
    sup: &ResidentSupervisor,
    engine: &SessionEngine,
    parent: SessionId,
    agent: &AgentSpec,
    directive: &str,
    sidecar: Option<Arc<dyn BoundSidecarFactory>>,
) -> (SessionId, String) {
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    sup.spawn_resident(
        parent,
        agent.clone(),
        (binding, Arc::from([]), resources, sidecar),
        directive.into(),
        None,
        None,
    )
    .await
    .unwrap()
}

async fn ensure_main_agent(
    sup: &ResidentSupervisor,
    engine: &SessionEngine,
    root: SessionId,
    agent: &AgentSpec,
) {
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let agents = engine
        .agent_roster_for_binding(&binding, "hya-main")
        .unwrap();
    let resources = engine
        .agent_resource_policy_for_binding(&binding, "hya-main")
        .unwrap();
    sup.ensure_main(
        root,
        agent.clone(),
        (binding, agents, resources),
        None,
        None,
    )
    .await
    .unwrap();
}

async fn is_active(engine: &SessionEngine, session: SessionId) -> bool {
    engine
        .store()
        .active_actor_ids()
        .await
        .unwrap()
        .contains(&session)
}

async fn assistant_turns(engine: &SessionEngine, session: SessionId) -> usize {
    engine
        .read_projection(session)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::Assistant))
        .count()
}

async fn roster_status(
    engine: &SessionEngine,
    root: SessionId,
    leaf: &str,
) -> Option<RosterStatus> {
    team_projection(&engine.read_projection(root).await.unwrap())
        .roster
        .get(&format!("main/{leaf}"))
        .map(|e| e.status)
}

fn member(engine: &SessionEngine, agent: &AgentSpec, directive: &str) -> ResidentTurn {
    ResidentTurn {
        agent: agent.clone(),
        binding: engine.bind_runtime(&agent.workdir).unwrap(),
        agents: Arc::from([]),
        resources: None,
        guidance: None,
        directive: directive.into(),
        description: String::new(),
        sidecar_factory: None,
        tool_call: None,
    }
}

async fn wait_activity(
    events: &mut broadcast::Receiver<Envelope>,
    root: SessionId,
    handle: &str,
    want: impl Fn(RosterStatus, Option<&str>) -> bool,
) -> (RosterStatus, Option<String>) {
    tokio::time::timeout(END_BOUND, async {
        loop {
            if let Some(ExtensionEvent::AgentActivityChanged {
                session,
                handle: h,
                status,
                current_task,
            }) = ExtensionEvent::from_event(&events.recv().await.unwrap().event)
                && session == root
                && h == handle
                && want(status, current_task.as_deref())
            {
                return (status, current_task);
            }
        }
    })
    .await
    .expect("expected resident activity transition")
}

async fn wait_loss_and_failure(
    terminated: &Notify,
    events: &mut broadcast::Receiver<Envelope>,
    root: SessionId,
    handle: &str,
) {
    tokio::time::timeout(END_BOUND, async {
        let (mut term_seen, mut fail_seen) = (false, false);
        while !(term_seen && fail_seen) {
            tokio::select! {
                _ = terminated.notified(), if !term_seen => term_seen = true,
                env = events.recv(), if !fail_seen => {
                    if matches!(
                        ExtensionEvent::from_event(&env.unwrap().event),
                        Some(ExtensionEvent::AgentActivityChanged {
                            session,
                            handle: h,
                            status: RosterStatus::Failed,
                            current_task: Some(t),
                        }) if session == root && h == handle && t.starts_with("activation sidecar lost")
                    ) {
                        fail_seen = true;
                    }
                }
            }
        }
    })
    .await
    .expect("running loss must finalize before provider release");
}

async fn assert_sidecar_failed(
    engine: &SessionEngine,
    sup: &ResidentSupervisor,
    root: SessionId,
    child: SessionId,
    handle: &str,
) {
    let events = engine.replay(root).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                ExtensionEvent::from_event(&e.event),
                Some(ExtensionEvent::SubagentReported { child: c, outcome: ReportOutcome::Failed, report, .. })
                    if c == child && report.contains("activation sidecar lost")
            ))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                ExtensionEvent::from_event(&e.event),
                Some(ExtensionEvent::MailSent { from, body, .. }) if from == handle && body.contains("activation sidecar lost")
            ))
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(ExtensionEvent::from_event(&e.event), Some(ExtensionEvent::AgentRestarted { child: c, .. }) if c == child))
    );
    assert_eq!(
        team_projection(&engine.read_projection(root).await.unwrap()).archived[handle].reason,
        ArchiveReason::Reported
    );
    assert!(!is_active(engine, child).await);
    assert_eq!(sup.episode_count(), 0);
}

async fn assert_revives(
    engine: &SessionEngine,
    root: SessionId,
    child: SessionId,
    handle: &str,
    body: &str,
) {
    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            root,
            MailEndpoint::parse(handle),
            MailKind::Message,
            body.into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(END_BOUND, async {
        loop {
            let has = engine
                .read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .iter()
                .any(|m| {
                    m.role == Role::User
                        && m.parts.iter().any(|p| matches!(p, PartProjection::Text { text, .. } if text.contains(body)))
                });
            if has {
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .expect("revived episode must admit committed mail");
    assert!(engine.replay(root).await.unwrap().iter().any(|e| matches!(
        ExtensionEvent::from_event(&e.event),
        Some(ExtensionEvent::AgentRestarted { child: c, epoch, .. }) if c == child && epoch > ActorEpoch::INITIAL
    )));
    let proj = engine.read_projection(root).await.unwrap();
    let team = team_projection(&proj);
    assert_eq!(team.roster[handle].session, child);
    assert!(!team.archived.contains_key(handle));
    assert!(is_active(engine, child).await);
}

async fn sidecar_bundle_engine(
    provider: Arc<dyn Provider>,
    mode: Mode,
    extra_bundles: Vec<Bundle>,
    plugin_hooks: Option<Arc<dyn NativeHook>>,
    sidecar: Option<Arc<dyn SidecarEnvironment>>,
) -> (Arc<SessionEngine>, AgentSpec) {
    let mut bundles = vec![sidecar_bundle()];
    bundles.extend(extra_bundles);
    let catalog = Arc::new(
        AgentCatalog::new(Arc::new(BundleCatalog::from_prepared(&bundles).unwrap())).unwrap(),
    );
    let runtime = support::builtin_test_runtime_with_catalog(catalog);
    if let Some(hooks) = plugin_hooks {
        runtime
            .runtime
            .refresh(|c| {
                c.upsert_sources(vec![
                    RuntimeSource::new(
                        RuntimeSourceId::bundle("acme/router"),
                        [9; 32],
                        Arc::new(()),
                        vec![],
                    )
                    .with_hooks(hooks),
                ])
            })
            .unwrap();
    }
    let mut engine = support::engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(provider)),
        runtime,
        PermissionRules::new(vec![Rule::new(Action::Tool, SIDECAR_TOOL, mode)]),
    )
    .await;
    if let Some(sidecar) = sidecar {
        engine = engine.with_sidecar_environment(sidecar);
    }
    (
        Arc::new(engine),
        support::agent_spec("sidecar-agent", "fake", empty_workdir()),
    )
}

// ---- Section 1: Observability isolation, directive recording, nested spawn ----

struct PerSessionRecorder {
    requests: Arc<Mutex<Vec<(SessionId, CompletionRequest)>>>,
    max_context: u32,
}
#[async_trait]
impl Provider for PerSessionRecorder {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m, self.max_context)
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let mut guard = self.requests.lock().unwrap();
        let first = !guard.iter().any(|(seen, _)| *seen == s);
        let directed = format!("{:?}", req.messages).contains("SECRET_DIRECTIVE_MARKER");
        guard.push((s, req));
        drop(guard);
        let steps = if first && directed {
            vec![
                FakeStep::ToolCall {
                    name: "todo".into(),
                    input: json!({"ops": []}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]
        } else {
            vec![
                FakeStep::Text("done".into()),
                FakeStep::Finish(FinishReason::Stop),
            ]
        };
        Ok(stream_steps(&steps, s, m))
    }
}

struct LeakProbeSummarizer;
#[async_trait]
impl Summarizer for LeakProbeSummarizer {
    async fn summarize(&self, _: &[Message], _: SummarizeOptions) -> Result<String, CoreError> {
        Ok("LEAK_PROBE_SUMMARY".into())
    }
}

#[tokio::test]
async fn observability_isolation_and_verbatim_spawn_directive() {
    let workdir = support::TestDir::new("observability-no-leak");
    let agents_path = workdir.path().join("AGENTS.md");
    std::fs::write(&agents_path, "CHILD_INITIAL_INSTRUCTIONS").unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(PerSessionRecorder {
        requests: requests.clone(),
        max_context: 30_000,
    });
    let engine = Arc::new(
        support::engine_with_provider(provider, PermissionRules::default())
            .await
            .with_terminal_handoff(
                Arc::new(LeakProbeSummarizer),
                TerminalHandoffOptions::default(),
            )
            .with_rewriters(support::compaction_rewriters(0)),
    );
    let agent = support::agent_spec("hya-main", "fake", workdir.path());
    let lead = support::create_root_session(&engine, "hya-main", "fake", workdir.path()).await;

    let tokenizer = hya_engine::tokens::TokenAccounting::default();
    let unit = "lorem ipsum dolor sit amet ".repeat(20);
    let directive = format!(
        "SECRET_DIRECTIVE_MARKER audit every provider route for retry handling. {}",
        unit.repeat(21_000 / tokenizer.tokenizer().count_text(&unit))
    );
    let call = ToolCallId::new();
    let mut spec = member(&engine, &agent, &directive);
    spec.description = "audit retries".into();
    spec.tool_call = Some(call);
    assert_eq!(
        run_residents(engine.clone(), lead, vec![spec], CancellationToken::new())
            .await
            .len(),
        1
    );

    let recorded = &members_projection(&engine.read_projection(lead).await.unwrap())[0];
    assert_eq!(
        (
            &recorded.directive,
            recorded.tool_call,
            recorded.description.as_str()
        ),
        (&directive, Some(call), "audit retries")
    );

    let child = recorded.child.unwrap();
    let child_events = engine.store().replay(child).await.unwrap();
    assert!(
        child_events
            .iter()
            .any(|e| matches!(e.event, Event::ContextCompacted { .. }))
    );
    assert!(child_events.iter().any(|e| matches!(
        &e.event,
        Event::InitialContextRecorded { session, text } if *session == child && text.contains("CHILD_INITIAL_INSTRUCTIONS")
    )));
    std::fs::write(&agents_path, "LEAD_INITIAL_INSTRUCTIONS").unwrap();

    engine
        .admit_user_prompt(lead, "what did the member find?".into())
        .await
        .unwrap();
    engine
        .run_turn(lead, &agent, CancellationToken::new())
        .await
        .unwrap();

    let requests = requests.lock().unwrap();
    assert!(requests.iter().any(|(s, r)| {
        *s == child
            && r.system
                .as_deref()
                .unwrap()
                .contains("CHILD_INITIAL_INSTRUCTIONS")
    }));
    let lead_reqs: Vec<_> = requests.iter().filter(|(s, _)| *s == lead).collect();
    assert!(!lead_reqs.is_empty());
    for (_, req) in lead_reqs {
        let sys = req.system.as_deref().unwrap();
        assert!(
            sys.contains("LEAD_INITIAL_INSTRUCTIONS")
                && !sys.contains("CHILD_INITIAL_INSTRUCTIONS")
        );
        let rendered = format!("{:?}", req.messages);
        for forbidden in [
            "HYA_COMPACTED_CONTEXT",
            "LEAK_PROBE_SUMMARY",
            "SECRET_DIRECTIVE_MARKER",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "{forbidden} leaked into lead input"
            );
        }
    }
}

#[tokio::test]
async fn resident_nested_spawn_completes_and_run_residents_rejects_failed_child() {
    let provider = Arc::new(FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "task".into(),
                input: json!({"description": "nested task", "prompt": "nested prompt", "subagent_type": "hya-task"}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("member complete".into()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]));
    let (spawn_tx, mut spawn_rx) = BoundSpawnSender::with_capacity(1);
    let engine = Arc::new(
        support::engine_with_provider(
            provider,
            PermissionRules::new(vec![Rule::new(Action::Task, "*", Mode::Allow)]),
        )
        .await
        .with_spawn_sender(spawn_tx),
    );
    let root = support::create_root_session(&engine, "hya-main", "fake", empty_workdir()).await;
    let binding = engine.bind_runtime(empty_workdir()).unwrap();
    let base = support::agent_spec("hya-main", "fake", empty_workdir());
    let turn = ResidentTurn {
        agent: engine
            .agent_spec_for_binding(&binding, &base, "hya-task")
            .unwrap(),
        agents: engine
            .agent_roster_for_binding(&binding, "hya-task")
            .unwrap(),
        resources: Some(binding.agent_resource_policy("hya-task").unwrap()),
        binding,
        guidance: None,
        directive: "member directive".into(),
        description: "member task".into(),
        sidecar_factory: None,
        tool_call: None,
    };
    let run = tokio::spawn(run_residents(
        engine,
        root,
        vec![turn],
        CancellationToken::new(),
    ));
    let (_, req) = tokio::time::timeout(Duration::from_secs(5), spawn_rx.recv())
        .await
        .unwrap()
        .unwrap()
        .into_parts();
    let nested_parent = req.parent;
    req.reply
        .send(Ok(vec![MemberOutcome {
            member: "nested-member".into(),
            session: SessionId::new().to_string(),
            status: "done".into(),
            summary: "nested complete".into(),
            model: None,
        }]))
        .unwrap();
    let evidence = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(evidence, vec![nested_parent]);

    // `run_residents` panics when a child turn ends with FinishReason::Error.
    let (fail_engine, fail_agent) = governed_engine(
        SubagentLimits::default(),
        Arc::new(FakeProvider::scripted(vec![FakeStep::Finish(
            FinishReason::Error,
        )])),
    )
    .await;
    let fail_root =
        support::create_root_session(&fail_engine, "hya-main", "fake", empty_workdir()).await;
    let turn = member(&fail_engine, &fail_agent, "must finish normally");
    assert!(
        tokio::spawn(run_residents(
            fail_engine,
            fail_root,
            vec![turn],
            CancellationToken::new()
        ))
        .await
        .is_err_and(|e| e.is_panic())
    );
}

// ---- Section 2: Governor stream partitions, concurrency cap, max depth ----

#[tokio::test]
async fn governor_enforces_stream_partitions_concurrency_and_max_depth() {
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let (engine, agent) = governed_engine(
        SubagentLimits::default(),
        Arc::new(PollGateProvider {
            entered: entered.clone(),
            release: release.clone(),
        }),
    )
    .await;
    let gov = engine.governor().unwrap().clone();
    assert_eq!(
        (
            gov.limits().max_concurrency,
            gov.available_general_stream_permits(),
            gov.available_reserved_stream_permits()
        ),
        (100, 100, 28)
    );
    let mut general = Vec::with_capacity(100);
    for _ in 0..100 {
        general.push(gov.acquire_general_stream().await.unwrap());
    }
    assert_eq!(
        (
            gov.available_general_stream_permits(),
            gov.available_reserved_stream_permits()
        ),
        (0, 28)
    );

    let root = support::create_root_session(&engine, "hya-main", "fake", &agent.workdir).await;
    engine
        .admit_user_prompt(root, "root progress".into())
        .await
        .unwrap();
    let run = tokio::spawn({
        let (engine, agent) = (engine.clone(), agent.clone());
        async move {
            engine
                .run_turn(root, &agent, CancellationToken::new())
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .unwrap();
    assert_eq!(
        (
            gov.available_general_stream_permits(),
            gov.available_reserved_stream_permits()
        ),
        (0, 27)
    );
    release.notify_one();
    assert_eq!(run.await.unwrap().unwrap(), FinishReason::Stop);
    assert_eq!(gov.available_reserved_stream_permits(), 28);

    let mut reserved = Vec::with_capacity(28);
    for _ in 0..28 {
        reserved.push(gov.acquire_reserved_stream().await.unwrap());
    }
    assert_eq!(gov.available_reserved_stream_permits(), 0);
    drop(general.pop());
    assert_eq!(
        (
            gov.available_general_stream_permits(),
            gov.available_reserved_stream_permits()
        ),
        (1, 0)
    );
    general.push(gov.acquire_general_stream().await.unwrap());
    drop(reserved.pop());
    assert_eq!(gov.available_reserved_stream_permits(), 1);
    reserved.push(gov.acquire_reserved_stream().await.unwrap());
    drop((general, reserved));
    assert_eq!(
        (
            gov.available_general_stream_permits(),
            gov.available_reserved_stream_permits()
        ),
        (100, 28)
    );

    // Streaming concurrency cap (max_concurrency = 2 across 6 residents).
    let (current, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (cap_engine, cap_agent) = governed_engine(
        SubagentLimits {
            max_concurrency: 2,
            per_run_budget: 100,
            ..Default::default()
        },
        Arc::new(ConcurrencyProbe {
            current,
            peak: peak.clone(),
        }),
    )
    .await;
    let lead =
        support::create_root_session(&cap_engine, "hya-main", "fake", &cap_agent.workdir).await;
    let specs: Vec<_> = (0..6)
        .map(|i| member(&cap_engine, &cap_agent, &format!("m{i}")))
        .collect();
    assert_eq!(
        run_residents(cap_engine.clone(), lead, specs, CancellationToken::new())
            .await
            .len(),
        6
    );
    assert!(peak.load(Ordering::SeqCst) <= 2);

    // Max depth rejection (depth 3 > MAX_SUBAGENT_DEPTH 2).
    let child = create_child(&cap_engine, lead, "hya-main", "fake").await;
    let grandchild = create_child(&cap_engine, child, "hya-main", "fake").await;
    let sup = ResidentSupervisor::start(cap_engine.clone());
    let binding = cap_engine.bind_runtime(&cap_agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("hya-main").unwrap();
    let err = sup
        .spawn_resident(
            grandchild,
            cap_agent,
            (binding, Arc::from([]), resources, None),
            "too deep".into(),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("depth"), "{err}");
}

// ---- Section 3: Sidecar ACK, permissions, tool bindings, and hook composition ----

#[tokio::test]
async fn sidecar_ack_precedes_running_state_for_initial_and_mailbox_wakes() {
    let polls = Arc::new(AtomicUsize::new(0));
    let (engine, agent) = governed_engine(
        SubagentLimits::default(),
        Arc::new(AckProbeProvider {
            polls: polls.clone(),
        }),
    )
    .await;
    let lead = support::create_root_session(&engine, "hya-main", "fake", &agent.workdir).await;
    let (start_tx, mut start_rx) = mpsc::unbounded_channel();
    let ready = Arc::new(Notify::new());
    let mut spec = member(&engine, &agent, "harness-owned task");
    spec.sidecar_factory = Some(Arc::new(AckGateFactory {
        starts: start_tx,
        ready: ready.clone(),
    }));
    let run = tokio::spawn(run_residents(
        engine.clone(),
        lead,
        vec![spec],
        CancellationToken::new(),
    ));
    let start = start_rx.recv().await.unwrap();
    assert!(
        !start.activation_id.is_empty() && matches!(start.lifecycle, SidecarLifecycle::Resident)
    );
    assert_eq!(polls.load(Ordering::SeqCst), 0);

    let row = members_projection(&engine.read_projection(lead).await.unwrap())[0].clone();
    assert_eq!(row.status, MemberRunStatus::Spawning);
    let child = row.child.unwrap();
    assert!(
        engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .is_empty()
    );
    ready.notify_one();
    assert_eq!(run.await.unwrap().len(), 1);
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert_eq!(support::assistant_text(&engine, child).await, "ACKTEXT");
    assert!(engine.read_projection(child).await.unwrap().session.messages.iter().any(|m| {
        m.role == Role::User && m.parts.iter().any(|p| matches!(p, PartProjection::Text { text, .. } if text == "[mail from main] harness-owned task"))
    }));

    // Parked resident mailbox wake waits for sidecar ready ACK before running.
    let (start_tx2, mut start_rx2) = mpsc::unbounded_channel();
    let ready2 = Arc::new(Notify::new());
    let sup = ResidentSupervisor::start(engine.clone());
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("hya-main").unwrap();
    let (parked, handle, _) = sup
        .spawn_resident_parked(
            lead,
            agent,
            (
                binding,
                Arc::from([]),
                resources,
                Some(Arc::new(AckGateFactory {
                    starts: start_tx2,
                    ready: ready2.clone(),
                })),
            ),
            "sidecar test".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(start_rx2.try_recv().is_err());
    assert_eq!(polls.load(Ordering::SeqCst), 1);

    send_mail(&engine, lead, &handle, "resident task").await;
    let start2 = start_rx2.recv().await.unwrap();
    assert!(
        !start2.activation_id.is_empty() && matches!(start2.lifecycle, SidecarLifecycle::Resident)
    );
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    assert!(
        engine
            .read_projection(parked)
            .await
            .unwrap()
            .session
            .messages
            .is_empty()
    );
    assert_eq!(
        team_projection(&engine.read_projection(lead).await.unwrap()).roster[&handle].status,
        RosterStatus::Idle
    );

    ready2.notify_one();
    assert!(support::poll_until(END_BOUND, || async { polls.load(Ordering::SeqCst) == 2 }).await);
    assert!(engine.read_projection(parked).await.unwrap().session.messages.iter().any(|m| {
        m.role == Role::User && m.parts.iter().any(|p| matches!(p, PartProjection::Text { text, .. } if text == "[mail from main] resident task"))
    }));
    sup.team_cancel(lead).unwrap().cancel();
}

#[tokio::test]
async fn sidecar_tool_binding_respects_permission_deny_and_allow_dispatch() {
    for allow in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let bindings: Arc<[ResolvedTool]> = Arc::from(vec![ResolvedTool {
            tool: Arc::new(SidecarProbeTool {
                name: SIDECAR_TOOL.into(),
                calls: calls.clone(),
                inputs: Arc::new(Mutex::new(vec![])),
            }),
            permission: ToolPermission::Tool,
        }]);
        let factory: Arc<dyn BoundSidecarFactory> = Arc::new(SidecarCfg {
            bindings,
            ..Default::default()
        });
        let requests = Arc::new(Mutex::new(vec![]));
        let (engine, agent) = sidecar_bundle_engine(
            Arc::new(SidecarEchoProvider {
                requests: requests.clone(),
                turn: AtomicUsize::new(0),
            }),
            if allow { Mode::Allow } else { Mode::Deny },
            vec![],
            None,
            None,
        )
        .await;
        let root =
            support::create_root_session(&engine, "sidecar-agent", "fake", &agent.workdir).await;

        let child = if !allow {
            let mut turn = member(&engine, &agent, "exercise denied sidecar tool");
            turn.resources = Some(turn.binding.agent_resource_policy("sidecar-agent").unwrap());
            turn.sidecar_factory = Some(factory);
            run_residents(engine.clone(), root, vec![turn], CancellationToken::new()).await[0]
        } else {
            let sup = ResidentSupervisor::start(engine.clone());
            let (child, handle) = spawn_res(&sup, &engine, root, &agent, "", Some(factory)).await;
            let mut events = engine.bus().subscribe();
            send_mail(&engine, root, &handle, "resident task").await;
            let (status, _) = wait_activity(&mut events, root, &handle, |s, _| {
                matches!(s, RosterStatus::Failed | RosterStatus::Idle)
            })
            .await;
            assert_eq!(status, RosterStatus::Idle);
            sup.team_cancel(root).unwrap().cancel();
            child
        };

        assert!(
            requests.lock().unwrap()[0]
                .tools
                .iter()
                .any(|s| s.name.as_str() == "echo")
        );
        let events = engine.store().replay(child).await.unwrap();
        if allow {
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(events.iter().any(|e| matches!(&e.event, Event::ToolResult { output, .. } if output == &json!({"ok": true}))));
        } else {
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(events.iter().any(|e| matches!(&e.event, Event::ToolError { message_text, .. } if message_text.contains("permission denied"))));
        }
    }
}

#[tokio::test]
async fn sidecar_hooks_mutate_tool_isolate_child_events_and_compose_with_plugin_hooks() {
    let plugin = prepare_package(BundleSource::new(
        "plugin",
        vec![SourceFile::new(
            "bundle.yaml",
            "schema_version: 2\nkind: Bundle\nidentity: { id: acme/router, version: 1.0.0, publisher: acme }\n",
        )],
    ))
    .unwrap();
    let inputs = Arc::new(Mutex::new(vec![]));
    let bindings: Arc<[ResolvedTool]> = Arc::from(vec![ResolvedTool {
        tool: Arc::new(SidecarProbeTool {
            name: SIDECAR_TOOL.into(),
            calls: Arc::new(AtomicUsize::new(0)),
            inputs: inputs.clone(),
        }),
        permission: ToolPermission::Tool,
    }]);
    let sidecar_hooks = Arc::new(ActivationHookProbe::default());
    let factory: Arc<dyn BoundSidecarFactory> = Arc::new(SidecarCfg {
        bindings,
        hooks: Some(sidecar_hooks.clone()),
        ..Default::default()
    });
    let plugin_hooks = PluginHookProbe::default();
    let (engine, agent) = sidecar_bundle_engine(
        Arc::new(SidecarEchoProvider {
            requests: Arc::new(Mutex::new(vec![])),
            turn: AtomicUsize::new(0),
        }),
        Mode::Allow,
        vec![plugin.bundles()[0].clone()],
        Some(Arc::new(plugin_hooks.clone())),
        None,
    )
    .await;
    let lead = support::create_root_session(&engine, "hya-main", "fake", &agent.workdir).await;
    let mut spec = member(&engine, &agent, "exercise plugin and sidecar hooks");
    spec.resources = Some(spec.binding.agent_resource_policy("sidecar-agent").unwrap());
    spec.sidecar_factory = Some(factory);
    let child = run_residents(engine.clone(), lead, vec![spec], CancellationToken::new()).await[0];

    let tool_results: Vec<_> = engine
        .store()
        .replay(child)
        .await
        .unwrap()
        .iter()
        .filter_map(|e| match &e.event {
            Event::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_results, vec![json!({"ok": true})]);
    assert_eq!(
        inputs.lock().unwrap().as_slice(),
        &[json!({"hooked": true})]
    );
    assert_eq!(
        (
            plugin_hooks.chat_params.load(Ordering::SeqCst),
            plugin_hooks.tool_before.load(Ordering::SeqCst)
        ),
        (2, 1)
    );
    assert_eq!(
        (
            sidecar_hooks.tool_before.load(Ordering::SeqCst),
            sidecar_hooks.tool_after.load(Ordering::SeqCst),
            sidecar_hooks.leak.load(Ordering::SeqCst)
        ),
        (1, 1, 0)
    );
    let captured = sidecar_hooks.events.lock().unwrap();
    assert!(!captured.is_empty() && captured.iter().all(|e| e.event.session() == Some(child)));
}

// ---- Section 4: Explicit stop, registration failure, fencing, budget, drift ----

#[tokio::test]
async fn explicit_idle_and_running_resident_stop_fences_drops_mail_and_revives() {
    // 1) Idle stop: final, idempotent, terminates sidecar once, releases claim, revives.
    let (engine, agent) = governed_engine(
        SubagentLimits::default(),
        Arc::new(StaticFake("MEMBERTEXT")),
    )
    .await;
    let root = support::create_root_session(&engine, "hya-main", "fake", &agent.workdir).await;
    let cfg = Arc::new(SidecarCfg {
        healthy: Arc::new(AtomicBool::new(false)),
        ..Default::default()
    });
    let sup = ResidentSupervisor::start(engine.clone());
    let (child, handle) = spawn_res(&sup, &engine, root, &agent, "", Some(cfg.clone())).await;
    let mut events = engine.bus().subscribe();
    send_mail(&engine, root, &handle, "resident stop probe").await;
    wait_activity(&mut events, root, &handle, |s, _| s == RosterStatus::Idle).await;
    assert_eq!(cfg.ready_calls.load(Ordering::SeqCst), 1);
    assert!(cfg.healthy.load(Ordering::SeqCst));

    sup.end_member(root, &handle).await.unwrap();
    sup.end_member(root, &handle).await.unwrap();
    assert_eq!(
        (
            cfg.shutdowns.load(Ordering::SeqCst),
            cfg.terminates.load(Ordering::SeqCst)
        ),
        (0, 1)
    );
    assert!(!is_active(&engine, child).await && sup.episode_count() == 0);
    let proj = engine.read_projection(root).await.unwrap();
    let team = team_projection(&proj);
    assert!(!team.roster.contains_key(&handle) && team.archived[&handle].session == child);
    assert_revives(&engine, root, child, &handle, "after idle stop").await;
    sup.end_member(root, &handle).await.unwrap();

    // 2) Running stop: concurrent second stop idempotent, drops queued mail, never commits
    //    provider text, wakes root synthesis, and revives cleanly.
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let (r_engine, r_agent) = governed_engine(
        SubagentLimits::default(),
        Arc::new(BarrierProvider {
            polls: Arc::new(AtomicUsize::new(0)),
            entered: entered_tx,
            release: release.clone(),
        }),
    )
    .await;
    let r_root =
        support::create_root_session(&r_engine, "hya-main", "fake", &r_agent.workdir).await;
    let r_cfg = Arc::new(SidecarCfg {
        healthy: Arc::new(AtomicBool::new(false)),
        ..Default::default()
    });
    let r_sup = ResidentSupervisor::start(r_engine.clone());
    let (r_child, r_handle) =
        spawn_res(&r_sup, &r_engine, r_root, &r_agent, "", Some(r_cfg.clone())).await;
    let mut r_events = r_engine.bus().subscribe();
    send_mail(&r_engine, r_root, &r_handle, "resident running stop").await;
    assert_eq!(entered_rx.recv().await, Some(r_child));
    send_mail(&r_engine, r_root, &r_handle, "queued after stop").await;

    let first_stop = tokio::spawn({
        let (s, h) = (r_sup.clone(), r_handle.clone());
        async move { s.end_member(r_root, &h).await }
    });
    tokio::time::timeout(END_BOUND, async {
        while !matches!(r_events.recv().await.unwrap().event, Event::MessageFinished { session, cause: Some(FinishCause::Archived), .. } if session == r_child) {}
    })
    .await
    .unwrap();
    let second_stop = r_sup.end_member(r_root, &r_handle);
    release.notify_one();
    first_stop.await.unwrap().unwrap();
    second_stop.await.unwrap();

    assert_eq!(
        tokio::time::timeout(END_BOUND, entered_rx.recv())
            .await
            .unwrap(),
        Some(r_root)
    );
    assert!(entered_rx.try_recv().is_err());
    assert_eq!(
        (
            r_cfg.terminates.load(Ordering::SeqCst),
            r_cfg.shutdowns.load(Ordering::SeqCst)
        ),
        (1, 0)
    );
    assert!(!is_active(&r_engine, r_child).await && r_sup.episode_count() == 0);
    let txt = format!(
        "{:?}",
        r_engine
            .read_projection(r_child)
            .await
            .unwrap()
            .session
            .messages
    );
    assert!(!txt.contains("queued after stop") && !txt.contains("RESIDENT_STOP_TEXT"));
    assert_revives(&r_engine, r_root, r_child, &r_handle, "after running stop").await;
}

#[tokio::test]
async fn resident_registration_failure_claim_fencing_and_pre_stop_mail_persistence() {
    let db_dir = support::TestDir::new("resident-reg-fail-and-stop");
    let db_str = db_dir
        .path()
        .join("sessions.db")
        .to_string_lossy()
        .into_owned();
    let open_engine = |store: SessionStore| {
        let (perm, _) = PermissionPlane::new(PermissionRules::default());
        Arc::new(
            SessionEngine::new(
                store,
                Arc::new(ProviderRouter::new().with(Arc::new(StaticFake("MEMBERTEXT")))),
                support::builtin_test_runtime().runtime,
                perm,
                EventBus::default(),
            )
            .with_round_retry_waits(&support::ZERO_RETRY_WAITS),
        )
    };
    let primary = open_engine(SessionStore::connect(&db_str).await.unwrap());
    let agent = support::agent_spec("hya-main", "fake", empty_workdir());
    let root = support::create_root_session(&primary, "hya-main", "fake", &agent.workdir).await;
    let failing = create_child(&primary, root, "hya-main", "fake").await;
    let sup = ResidentSupervisor::start(primary.clone());

    let mut conn = SqliteConnection::connect(&format!("sqlite://{db_str}"))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_reg BEFORE INSERT ON event_log \
         WHEN instr(NEW.payload, 'registration-failure-1') > 0 \
         BEGIN SELECT RAISE(ABORT, 'test registration append failure'); END;",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    assert!(
        sup.register_existing_resident(
            root,
            failing,
            "registration-failure-1".into(),
            agent.clone(),
            None
        )
        .await
        .is_err()
    );
    assert!(!is_active(&primary, failing).await && sup.team_cancel(root).is_none());
    assert!(!primary.replay(root).await.unwrap().iter().any(|e| matches!(ExtensionEvent::from_event(&e.event), Some(ExtensionEvent::AgentRegistered { handle, .. }) if handle == "registration-failure-1")));
    sqlx::query("DROP TRIGGER fail_reg")
        .execute(&mut conn)
        .await
        .unwrap();

    // Direct mail committed from a second connection before stop remains unread (cursor 0).
    let (child, handle) = spawn_res(&sup, &primary, root, &agent, "", None).await;
    let sender = open_engine(SessionStore::connect(&db_str).await.unwrap());
    sender
        .admit_user_prompt(root, "admit sender binding".into())
        .await
        .unwrap();
    let ready = Arc::new(Barrier::new(2));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn({
        let (ready, sender, handle) = (ready.clone(), sender.clone(), handle.clone());
        async move {
            ready.wait().await;
            let _ = tx.send(
                sender
                    .mail_send(
                        root,
                        MailEndpoint::Handle(handle),
                        MailKind::Message,
                        "mail accepted before stop".into(),
                    )
                    .await,
            );
        }
    });
    ready.wait().await;
    assert_eq!(rx.recv().await.unwrap().unwrap().recipients, 2);
    sup.end_member(root, &handle).await.unwrap();
    task.await.unwrap();
    assert!(!is_active(&primary, child).await);
    let mut before_archive = hya_proto::Projection::default();
    let reducers = projection_reducers();
    for env in primary.replay(root).await.unwrap() {
        if matches!(ExtensionEvent::from_event(&env.event), Some(ExtensionEvent::AgentArchived { handle: a, .. }) if a == handle)
        {
            break;
        }
        before_archive.apply_with(&env, &reducers);
    }
    let before_team = team_projection(&before_archive);
    assert_eq!(
        (
            before_team.inboxes[&handle].len(),
            before_team.roster[&handle].resident_cursor
        ),
        (1, 0)
    );

    // Explicit stop fails closed when a newer claim took over the resident.
    let (fenced, fenced_handle) = spawn_res(&sup, &primary, root, &agent, "", None).await;
    primary
        .store()
        .claim_runtime_owner(primary.runtime_owner())
        .unwrap();
    let newer = primary
        .store()
        .recover_claim(fenced, primary.runtime_owner())
        .await
        .unwrap();
    assert!(matches!(
        sup.end_member(root, &fenced_handle).await,
        Err(CoreError::Store(hya_store::StoreError::StaleBundleClaim { claim_key, .. })) if claim_key == fenced.to_string()
    ));
    assert!(
        primary
            .store()
            .validate_actor_claim(&newer.claim)
            .await
            .is_ok()
    );
    assert_eq!(
        team_projection(&primary.read_projection(root).await.unwrap()).roster[&fenced_handle]
            .status,
        RosterStatus::Idle
    );
}

#[tokio::test]
async fn message_budget_kill_terminates_sidecar_and_removes_resident_slot() {
    let (engine, agent) = governed_engine(
        SubagentLimits {
            per_team_message_budget: 1,
            ..Default::default()
        },
        Arc::new(StaticFake("MEMBERTEXT")),
    )
    .await;
    let root = support::create_root_session(&engine, "hya-main", "fake", &agent.workdir).await;
    let terminated = Arc::new(Notify::new());
    let cfg = Arc::new(SidecarCfg {
        healthy: Arc::new(AtomicBool::new(false)),
        terminated: Some(terminated.clone()),
        ..Default::default()
    });
    let sup = ResidentSupervisor::start(engine.clone());
    let (child, handle) = spawn_res(&sup, &engine, root, &agent, "", Some(cfg.clone())).await;
    let mut events = engine.bus().subscribe();
    send_mail(&engine, root, &handle, "first message").await;
    wait_activity(&mut events, root, &handle, |s, _| s == RosterStatus::Idle).await;
    assert_eq!(cfg.ready_calls.load(Ordering::SeqCst), 1);

    send_mail(&engine, root, &handle, "budget trip").await;
    wait_activity(&mut events, root, &handle, |s, t| {
        s == RosterStatus::Failed && t.is_some_and(|t| t.contains("message budget exceeded"))
    })
    .await;
    tokio::time::timeout(Duration::from_millis(250), terminated.notified())
        .await
        .unwrap();
    assert_eq!(
        (
            cfg.terminates.load(Ordering::SeqCst),
            cfg.shutdowns.load(Ordering::SeqCst)
        ),
        (1, 0)
    );
    assert!(!is_active(&engine, child).await && sup.team_cancel(root).is_none());
    let proj = engine.read_projection(root).await.unwrap();
    let team = team_projection(&proj);
    assert!(!team.roster.contains_key(&handle) && team.archived[&handle].session == child);
}

#[tokio::test]
async fn resident_replacement_declaration_drift_disables_once_and_revives() {
    let polls = Arc::new(AtomicUsize::new(0));
    let (engine, agent) = governed_engine(
        SubagentLimits::default(),
        Arc::new(AckProbeProvider {
            polls: polls.clone(),
        }),
    )
    .await;
    let root = support::create_root_session(&engine, "hya-main", "fake", &agent.workdir).await;
    let _lead = engine.try_begin_turn(root).unwrap();
    let cfg = Arc::new(SidecarCfg {
        drift_after_first: true,
        ..Default::default()
    });
    let sup = ResidentSupervisor::start(engine.clone());
    let (child, handle) = spawn_res(&sup, &engine, root, &agent, "", Some(cfg.clone())).await;
    let mut events = engine.bus().subscribe();
    send_mail(&engine, root, &handle, "resident declaration drift first").await;
    wait_activity(&mut events, root, &handle, |s, _| s == RosterStatus::Idle).await;
    assert_eq!(
        (
            cfg.starts.load(Ordering::SeqCst),
            polls.load(Ordering::SeqCst)
        ),
        (1, 1)
    );
    cfg.healthy.store(false, Ordering::SeqCst);

    send_mail(&engine, root, &handle, "resident declaration drift second").await;
    wait_activity(&mut events, root, &handle, |s, t| {
        s == RosterStatus::Failed
            && t.is_some_and(|t| t.contains("bundle sidecar declaration drift"))
    })
    .await;
    assert_eq!(
        (
            cfg.starts.load(Ordering::SeqCst),
            cfg.terminates.load(Ordering::SeqCst),
            polls.load(Ordering::SeqCst)
        ),
        (2, 1, 1)
    );
    assert!(!is_active(&engine, child).await && sup.episode_count() == 0);
    assert!(
        !format!(
            "{:?}",
            engine
                .read_projection(child)
                .await
                .unwrap()
                .session
                .messages
        )
        .contains("declaration drift second")
    );
    assert_revives(
        &engine,
        root,
        child,
        &handle,
        "resident declaration drift third",
    )
    .await;
}

// ---- Section 5: Sidecar loss paths (hooks, ready, idle, prework, running) ----

#[tokio::test]
async fn resident_sidecar_hook_transport_loss_all_stages() {
    for (stage, want_calls, want_reqs) in [
        (HookLossStage::Before, 0usize, 1usize),
        (HookLossStage::After, 1, 1),
        (HookLossStage::Event, 0, 0),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let bindings: Arc<[ResolvedTool]> = Arc::from(vec![ResolvedTool {
            tool: Arc::new(SidecarProbeTool {
                name: SIDECAR_TOOL.into(),
                calls: calls.clone(),
                inputs: Arc::new(Mutex::new(vec![])),
            }),
            permission: ToolPermission::Tool,
        }]);
        let (healthy, hook_calls) = (
            Arc::new(AtomicBool::new(true)),
            Arc::new(AtomicUsize::new(0)),
        );
        let cfg = Arc::new(SidecarCfg {
            bindings,
            hooks: Some(Arc::new(HookLossDispatcher {
                stage,
                healthy: healthy.clone(),
                calls: hook_calls.clone(),
            })),
            healthy,
            ..Default::default()
        });
        // The restarted episode gets a new, healthy sidecar with the same tools.
        let revived = Arc::new(SidecarCfg {
            bindings: cfg.bindings.clone(),
            ..Default::default()
        });
        let requests = Arc::new(Mutex::new(vec![]));
        let (engine, agent) = sidecar_bundle_engine(
            Arc::new(SidecarEchoProvider {
                requests: requests.clone(),
                turn: AtomicUsize::new(0),
            }),
            Mode::Allow,
            vec![],
            None,
            Some(revived.clone()),
        )
        .await;
        let root =
            support::create_root_session(&engine, "sidecar-agent", "fake", &agent.workdir).await;
        let sup = ResidentSupervisor::start(engine.clone());
        let (child, handle) = spawn_res(&sup, &engine, root, &agent, "", Some(cfg.clone())).await;
        let mut events = engine.bus().subscribe();
        send_mail(&engine, root, &handle, "resident hook loss").await;
        let (status, task) = wait_activity(&mut events, root, &handle, |s, _| {
            matches!(s, RosterStatus::Failed | RosterStatus::Idle)
        })
        .await;
        assert_eq!(
            (status, task.as_deref()),
            (RosterStatus::Failed, Some("activation sidecar lost"))
        );
        assert_eq!(
            (
                hook_calls.load(Ordering::SeqCst),
                calls.load(Ordering::SeqCst),
                requests.lock().unwrap().len(),
                cfg.terminates.load(Ordering::SeqCst)
            ),
            (1, want_calls, want_reqs, 1)
        );
        assert_sidecar_failed(&engine, &sup, root, child, &handle).await;
        let child_events = engine.store().replay(child).await.unwrap();
        assert!(
            !child_events
                .iter()
                .any(|e| matches!(&e.event, Event::ToolResult { .. }))
        );
        assert!(!child_events.iter().any(|e| matches!(&e.event, Event::ToolError { message_text, .. } if message_text.contains("blocked by plugin"))));
        assert_revives(&engine, root, child, &handle, "after hook loss").await;
        assert_eq!(revived.starts.load(Ordering::SeqCst), 1);
        sup.end_member(root, &handle).await.unwrap();
    }
}

#[tokio::test]
async fn resident_sidecar_ready_failure_and_idle_loss_reap() {
    let (engine, agent) = governed_engine(
        SubagentLimits::default(),
        Arc::new(StaticFake("MEMBERTEXT")),
    )
    .await;
    let root = support::create_root_session(&engine, "hya-main", "fake", &agent.workdir).await;
    let sup = ResidentSupervisor::start(engine.clone());
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("hya-main").unwrap();

    // 1) Ready failure terminates once and removes slot + claim.
    let fail_cfg = Arc::new(SidecarCfg {
        fail_ready: true,
        ..Default::default()
    });
    let (child, handle, _) = sup
        .spawn_resident_parked(
            root,
            agent.clone(),
            (binding, Arc::from([]), resources, Some(fail_cfg.clone())),
            "sidecar test".into(),
            None,
            None,
        )
        .await
        .unwrap();
    let mut events = engine.bus().subscribe();
    send_mail(&engine, root, &handle, "resident task").await;
    wait_activity(&mut events, root, &handle, |s, t| {
        s == RosterStatus::Failed && t.is_some_and(|t| t.contains("sidecar ACK rejected"))
    })
    .await;
    assert_eq!(
        (
            fail_cfg.ready_calls.load(Ordering::SeqCst),
            fail_cfg.terminates.load(Ordering::SeqCst),
            fail_cfg.shutdowns.load(Ordering::SeqCst)
        ),
        (1, 1, 0)
    );
    assert!(!is_active(&engine, child).await && sup.episode_count() == 0);

    // 2) Idle sidecar loss reaps sidecar before next mail while retaining slot & claim.
    let (terminated, loss) = (Arc::new(Notify::new()), CancellationToken::new());
    let idle_cfg = Arc::new(SidecarCfg {
        healthy: Arc::new(AtomicBool::new(false)),
        terminated: Some(terminated.clone()),
        loss: Some(loss.clone()),
        ..Default::default()
    });
    let (idle_child, idle_handle) =
        spawn_res(&sup, &engine, root, &agent, "", Some(idle_cfg.clone())).await;
    send_mail(&engine, root, &idle_handle, "first message").await;
    wait_activity(&mut events, root, &idle_handle, |s, _| {
        s == RosterStatus::Idle
    })
    .await;
    loss.cancel();
    tokio::time::timeout(Duration::from_millis(250), terminated.notified())
        .await
        .unwrap();
    assert_eq!(idle_cfg.terminates.load(Ordering::SeqCst), 1);
    assert!(sup.team_cancel(root).is_some() && is_active(&engine, idle_child).await);
    sup.team_cancel(root).unwrap().cancel();
}

#[tokio::test]
async fn resident_prework_sidecar_loss_preserves_pending_mail_and_revives() {
    for initial in [false, true] {
        let polls = Arc::new(AtomicUsize::new(0));
        let (engine, agent) = governed_engine(
            SubagentLimits::default(),
            Arc::new(AckProbeProvider {
                polls: polls.clone(),
            }),
        )
        .await;
        let root = support::create_root_session(&engine, "hya-main", "fake", &agent.workdir).await;
        let (terminated, first_loss) = (Arc::new(Notify::new()), CancellationToken::new());
        first_loss.cancel();
        let cfg = Arc::new(SidecarCfg {
            terminated: Some(terminated.clone()),
            loss: Some(first_loss),
            loss_first_only: true,
            ..Default::default()
        });
        let sup = ResidentSupervisor::start(engine.clone());
        let mut events = engine.bus().subscribe();
        let directive = if initial {
            "initial directive survives"
        } else {
            "rearm after prework loss"
        };
        let (child, handle) = spawn_res(
            &sup,
            &engine,
            root,
            &agent,
            if initial { directive } else { "" },
            Some(cfg.clone()),
        )
        .await;
        if !initial {
            send_mail(&engine, root, &handle, directive).await;
        }
        tokio::time::timeout(END_BOUND, terminated.notified())
            .await
            .unwrap();
        tokio::time::timeout(END_BOUND, async {
            while !matches!(ExtensionEvent::from_event(&events.recv().await.unwrap().event), Some(ExtensionEvent::AgentArchived { child: c, .. }) if c == child) {}
        })
        .await
        .unwrap();
        assert_eq!(
            (
                cfg.starts.load(Ordering::SeqCst),
                cfg.terminates.load(Ordering::SeqCst),
                polls.load(Ordering::SeqCst)
            ),
            (1, 1, 0)
        );
        assert_sidecar_failed(&engine, &sup, root, child, &handle).await;
        let proj = engine.read_projection(child).await.unwrap();
        assert!(proj.session.messages.is_empty());
        assert!(
            handoff_projection(&proj)
                .unwrap()
                .doc
                .contains(&format!("5. Pending tasks - [mail from main] {directive}"))
        );
        assert_revives(&engine, root, child, &handle, "after prework loss").await;
        sup.end_member(root, &handle).await.unwrap();
        assert_eq!(cfg.shutdowns.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn running_resident_sidecar_loss_aborts_and_handles_terminate_failure() {
    for fail_terminate in [false, true] {
        let (polls, release) = (Arc::new(AtomicUsize::new(0)), Arc::new(Notify::new()));
        let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
        let (engine, agent) = governed_engine(
            SubagentLimits::default(),
            Arc::new(BarrierProvider {
                polls: polls.clone(),
                entered: entered_tx,
                release: release.clone(),
            }),
        )
        .await;
        let root = support::create_root_session(&engine, "hya-main", "fake", &agent.workdir).await;
        let (terminated, loss) = (Arc::new(Notify::new()), CancellationToken::new());
        let cfg = Arc::new(SidecarCfg {
            healthy: Arc::new(AtomicBool::new(false)),
            terminated: Some(terminated.clone()),
            loss: Some(loss.clone()),
            fail_terminate,
            ..Default::default()
        });
        let sup = ResidentSupervisor::start(engine.clone());
        let (child, handle) = spawn_res(&sup, &engine, root, &agent, "", Some(cfg.clone())).await;
        let mut events = engine.bus().subscribe();
        send_mail(&engine, root, &handle, "running loss").await;
        assert_eq!(entered_rx.recv().await, Some(child));
        loss.cancel();
        wait_loss_and_failure(&terminated, &mut events, root, &handle).await;
        assert_eq!(
            (cfg.terminates.load(Ordering::SeqCst), sup.episode_count()),
            (1, 0)
        );
        assert!(!is_active(&engine, child).await);
        release.notify_one();
        let _ = sup.end_member(root, &handle).await;
        assert!(!engine.store().replay(child).await.unwrap().iter().any(|e| matches!(&e.event, Event::TextReplace { text, .. } if text.contains("RESIDENT_STOP_TEXT"))));
        // Only the child's re-entry counts: the root may take its quiescence
        // synthesis turn through the same provider.
        let reentered: Vec<_> = std::iter::from_fn(|| entered_rx.try_recv().ok()).collect();
        assert!(
            !reentered.contains(&child),
            "child polled again: {reentered:?}"
        );
        assert_sidecar_failed(&engine, &sup, root, child, &handle).await;
        if fail_terminate {
            let proj = engine.read_projection(root).await.unwrap();
            let members = members_projection(&proj);
            let summary = &members
                .iter()
                .find(|m| m.child == Some(child))
                .unwrap()
                .summary;
            assert!(summary.contains("sidecar did not stop"));
        }
        assert_revives(&engine, root, child, &handle, "after running loss").await;
        sup.end_member(root, &handle).await.unwrap();
    }
}

#[tokio::test]
async fn running_resident_sidecar_loss_never_takes_over_claim_or_runs_recovery_tx() {
    let db_dir = support::TestDir::new("resident-running-loss-fencing");
    let db_str = db_dir
        .path()
        .join("sessions.db")
        .to_string_lossy()
        .into_owned();
    let (polls, release) = (Arc::new(AtomicUsize::new(0)), Arc::new(Notify::new()));
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel();
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect(&db_str).await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(BarrierProvider {
                polls: polls.clone(),
                entered: entered_tx,
                release: release.clone(),
            }))),
            support::builtin_test_runtime().runtime,
            perm,
            EventBus::default(),
        )
        .with_governor(SubagentGovernor::new(SubagentLimits::default()))
        .with_round_retry_waits(&support::ZERO_RETRY_WAITS),
    );
    let agent = support::agent_spec("hya-main", "fake", empty_workdir());
    let root = support::create_root_session(&engine, "hya-main", "fake", &agent.workdir).await;
    let (terminated, loss) = (Arc::new(Notify::new()), CancellationToken::new());
    let cfg = Arc::new(SidecarCfg {
        healthy: Arc::new(AtomicBool::new(false)),
        terminated: Some(terminated.clone()),
        loss: Some(loss.clone()),
        ..Default::default()
    });
    let sup = ResidentSupervisor::start(engine.clone());
    let (child, handle) = spawn_res(&sup, &engine, root, &agent, "", Some(cfg.clone())).await;
    let mut events = engine.bus().subscribe();
    send_mail(&engine, root, &handle, "running loss fencing").await;
    assert_eq!(entered_rx.recv().await, Some(child));

    let mut conn = SqliteConnection::connect(&format!("sqlite://{db_str}"))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_recover_claim BEFORE UPDATE OF epoch, owner ON bundle_claim \
         WHEN OLD.state = 'active' AND NEW.state = 'active' AND NEW.epoch = OLD.epoch + 1 \
         BEGIN SELECT RAISE(ABORT, 'test recover claim boundary'); END;",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    sqlx::query(&format!(
        "CREATE TRIGGER reject_recovery_tx BEFORE INSERT ON event_log \
         WHEN instr(NEW.payload, '\"kind\":\"agent_activity_changed\"') > 0 \
           AND instr(NEW.payload, '\"session\":\"{root}\"') > 0 \
           AND instr(NEW.payload, '\"current_task\":\"aborted by resident recovery\"') > 0 \
         BEGIN SELECT RAISE(ABORT, 'test recovery transaction boundary'); END;"
    ))
    .execute(&mut conn)
    .await
    .unwrap();
    loss.cancel();
    wait_loss_and_failure(&terminated, &mut events, root, &handle).await;

    let child_at_failure = engine.store().replay(child).await.unwrap();
    release.notify_one();
    let root_events = engine.store().replay(root).await.unwrap();
    assert_eq!(
        root_events
            .iter()
            .filter(|e| matches!(
                ExtensionEvent::from_event(&e.event),
                Some(ExtensionEvent::AgentActivityChanged { handle: h, status: RosterStatus::Failed, current_task: Some(t), .. })
                    if h == handle && t.starts_with("activation sidecar lost")
            ))
            .count(),
        1
    );
    assert!(!root_events.iter().any(|e| matches!(
        ExtensionEvent::from_event(&e.event),
        Some(ExtensionEvent::AgentActivityChanged { current_task: Some(t), .. }) if t == "aborted by resident recovery"
    )));
    assert_eq!(
        (
            cfg.terminates.load(Ordering::SeqCst),
            polls.load(Ordering::SeqCst)
        ),
        (1, 1)
    );
    while let Ok(s) = entered_rx.try_recv() {
        assert_ne!(s, child);
    }
    assert_eq!(
        engine.store().replay(child).await.unwrap(),
        child_at_failure
    );
    assert_sidecar_failed(&engine, &sup, root, child, &handle).await;
    assert_revives(&engine, root, child, &handle, "after fenced loss").await;
    sup.end_member(root, &handle).await.unwrap();
}

// ---- Section 6: Resident lifecycle (wake/idle, quiescence, budget, recovery) ----

#[tokio::test]
async fn resident_wakes_for_one_turn_quiescence_synthesizes_and_report_notice_owes_wake() {
    // 1) Born idle runs 0 turns; 1 inbound mail wakes for exactly 1 turn then idles.
    let (engine, agent) = dev_engine(SubagentLimits::default()).await;
    let root =
        support::create_root_session(&engine, "hya-main", "hya/offline", &agent.workdir).await;
    let worker = create_child(&engine, root, "hya-task", "hya/offline").await;
    let sup = ResidentSupervisor::start(engine.clone());
    sup.register_existing_resident(root, worker, "worker-1".into(), agent.clone(), None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;
    assert_eq!(assistant_turns(&engine, worker).await, 0);
    send_mail(&engine, root, "worker-1", "please look at this").await;
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            assistant_turns(&engine, worker).await == 1
                && roster_status(&engine, root, "worker-1").await == Some(RosterStatus::Idle)
        })
        .await
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(assistant_turns(&engine, worker).await, 1);

    // 2) Quiescence wakes main to synthesize once without user input.
    let (q_engine, q_agent) = dev_engine(SubagentLimits::default()).await;
    let q_root =
        support::create_root_session(&q_engine, "hya-main", "hya/offline", &q_agent.workdir).await;
    let q_sup = ResidentSupervisor::start(q_engine.clone());
    ensure_main_agent(&q_sup, &q_engine, q_root, &q_agent).await;
    for (handle, prompt) in [("worker-1", "do part A"), ("worker-2", "do part B")] {
        let c = create_child(&q_engine, q_root, "hya-task", "hya/offline").await;
        q_sup
            .register_existing_resident(
                q_root,
                c,
                handle.into(),
                q_agent.clone(),
                Some(prompt.into()),
            )
            .await
            .unwrap();
    }
    assert!(support::poll_until(Duration::from_secs(5), || async {
        let p = q_engine.read_projection(q_root).await.unwrap();
        p.session.messages.iter().any(|m| {
            m.role == Role::User && m.parts.iter().any(|pt| matches!(pt, PartProjection::Text { text, .. } if text.contains("TEAM QUIESCED")))
        }) && p.session.messages.iter().any(|m| m.role == Role::Assistant && m.finish.is_some())
    }).await);
    assert_eq!(
        roster_status(&q_engine, q_root, "worker-1").await,
        Some(RosterStatus::Idle)
    );
    assert_eq!(
        roster_status(&q_engine, q_root, "worker-2").await,
        Some(RosterStatus::Idle)
    );
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(assistant_turns(&q_engine, q_root).await <= 3);

    // 3) Preparing a report steer notice without committing leaves the lead wake owed.
    let (n_engine, n_agent) = dev_engine(SubagentLimits::default()).await;
    let n_root =
        support::create_root_session(&n_engine, "hya-main", "hya/offline", &n_agent.workdir).await;
    let n_sup = ResidentSupervisor::start(n_engine.clone());
    let lease = n_engine.try_begin_turn(n_root).unwrap();
    ensure_main_agent(&n_sup, &n_engine, n_root, &n_agent).await;
    let n_child = create_child(&n_engine, n_root, "hya-task", "hya/offline").await;
    n_sup
        .register_existing_resident(n_root, n_child, "worker-1".into(), n_agent, None)
        .await
        .unwrap();
    send_mail(&n_engine, n_child, "main", "worker failed; no result").await;
    n_sup
        .archive_member(n_root, "worker-1", "failed")
        .await
        .unwrap();
    assert!(
        support::steer_notice(&n_engine, n_root)
            .await
            .unwrap()
            .contains("worker failed; no result")
    );
    drop(lease);
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            assistant_turns(&n_engine, n_root).await == 1
        })
        .await
    );
}

#[tokio::test]
async fn resident_message_budget_kill_cancels_team_and_archives_members() {
    let (engine, agent) = dev_engine(SubagentLimits {
        per_team_message_budget: 3,
        ..Default::default()
    })
    .await;
    let root =
        support::create_root_session(&engine, "hya-main", "hya/offline", &agent.workdir).await;
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main_agent(&sup, &engine, root, &agent).await;
    let a = create_child(&engine, root, "hya-task", "hya/offline").await;
    let b = create_child(&engine, root, "hya-task", "hya/offline").await;
    sup.register_existing_resident(root, a, "worker-1".into(), agent.clone(), None)
        .await
        .unwrap();
    sup.register_existing_resident(root, b, "worker-2".into(), agent, None)
        .await
        .unwrap();

    let cancel = sup.team_cancel(root).unwrap();
    assert!(!cancel.is_cancelled());
    for i in 0..4 {
        let (from, to) = if i % 2 == 0 {
            (root, "worker-1")
        } else {
            (a, "main")
        };
        let _ = engine
            .mail_send(
                from,
                MailEndpoint::Handle(to.into()),
                MailKind::Message,
                format!("ping {i}"),
            )
            .await;
    }
    tokio::time::timeout(Duration::from_secs(5), cancel.cancelled())
        .await
        .unwrap();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            let p = engine.read_projection(root).await.unwrap();
            let team = team_projection(&p);
            ["main/worker-1", "main/worker-2"]
                .iter()
                .all(|h| !team.roster.contains_key(*h) && team.archived.contains_key(*h))
        })
        .await
    );
}

#[tokio::test]
async fn resident_initial_task_survives_claim_takeover_and_parent_dm_policy() {
    // 1) Initial task survives claim takeover before first turn.
    let (engine, agent) = dev_engine(SubagentLimits::default()).await;
    let root =
        support::create_root_session(&engine, "hya-main", "hya/offline", &agent.workdir).await;
    let root_turn = engine.try_begin_turn(root).unwrap();
    let child = create_child(&engine, root, "hya-task", "hya/offline").await;
    let child_turn = engine.try_begin_turn(child).unwrap();
    let handle = "main/worker-1".to_string();
    let sup = ResidentSupervisor::start(engine.clone());
    sup.register_existing_resident(
        root,
        child,
        "worker-1".into(),
        agent.clone(),
        Some("durable initial task".into()),
    )
    .await
    .unwrap();
    let prev_cancel = sup.team_cancel(root).unwrap();
    assert_eq!(assistant_turns(&engine, child).await, 0);
    assert!(
        team_projection(&engine.read_projection(root).await.unwrap()).roster[&handle]
            .resident_work
            .is_none()
    );

    engine
        .store()
        .claim_runtime_owner(engine.runtime_owner())
        .unwrap();
    let recovered = engine
        .store()
        .recover_claim(child, engine.runtime_owner())
        .await
        .unwrap();
    let disp = engine
        .recover_resident_actor(&recovered, root, &handle)
        .await
        .unwrap();
    assert!(matches!(disp.work, ResidentRecovery::Queued { .. }));
    prev_cancel.cancel();
    drop(sup);
    let sup = ResidentSupervisor::start(engine.clone());
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("hya-task").unwrap();
    sup.register_recovered_resident(
        root,
        handle,
        agent,
        (binding, Arc::from([]), resources, None),
        recovered,
        disp.work,
    )
    .await
    .unwrap();
    drop(child_turn);
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            support::assistant_text(&engine, child)
                .await
                .contains("durable initial task")
        })
        .await
    );
    drop(root_turn);
    sup.drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;

    // 2) Spawn directive + follow-up deliver over parent DM even under restrictive child policy.
    let pkg = prepare_package(BundleSource::new(
        "deny-dm-wakes",
        vec![SourceFile::new(
            "bundle.yaml",
            "schema_version: 2\nkind: Bundle\nidentity: { id: test/deny-dm-wakes, version: 1.0.0, publisher: test }\nchannels:\n  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [], scope: vertical, retention: team_session }\n",
        )],
    ))
    .unwrap();
    let multi_agent = hya_bundle::first_party_bundle("hya/multi-agent").unwrap();
    let catalog = Arc::new(
        AgentCatalog::new(Arc::new(
            BundleCatalog::from_verified_catalogs(&[&pkg, multi_agent, support::core_agents()])
                .unwrap(),
        ))
        .unwrap(),
    );
    let p_engine = Arc::new(
        support::engine_with_router_and_runtime(
            Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
            support::builtin_test_runtime_with_catalog(catalog),
            PermissionRules::default(),
        )
        .await,
    );
    let p_root =
        support::create_root_session(&p_engine, "hya-main", "hya/offline", empty_workdir()).await;
    let p_root_turn = p_engine.try_begin_turn(p_root).unwrap();
    let p_child = create_child(&p_engine, p_root, "hya-task", "hya/offline").await;
    let p_child_turn = p_engine.try_begin_turn(p_child).unwrap();
    let p_sup = ResidentSupervisor::start(p_engine.clone());
    p_sup
        .register_existing_resident(
            p_root,
            p_child,
            "worker".into(),
            support::agent_spec("hya-task", "hya/offline", empty_workdir()),
            Some("inspect parser".into()),
        )
        .await
        .unwrap();
    let proj = p_engine.read_projection(p_root).await.unwrap();
    let p_team = team_projection(&proj);
    let mail = &p_team.mail[p_team.inboxes["main/worker"][0]];
    assert_eq!(
        (mail.from.as_str(), mail.body.as_str()),
        ("main", "inspect parser")
    );
    let MailEndpoint::Channel(dm) = &mail.to else {
        panic!("directive must name its DM channel");
    };
    assert_eq!(p_team.channels[dm].kind, ChannelKind::Dm);
    assert!(
        p_engine
            .read_projection(p_child)
            .await
            .unwrap()
            .session
            .messages
            .is_empty()
    );
    p_engine
        .mail_send(
            p_root,
            MailEndpoint::Channel(dm.clone()),
            MailKind::Message,
            "also inspect tests".into(),
        )
        .await
        .unwrap();
    drop(p_child_turn);
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            assistant_turns(&p_engine, p_child).await >= 1
                && team_projection(&p_engine.read_projection(p_root).await.unwrap()).roster["main/worker"]
                    .resident_work
                    .is_none()
        })
        .await
    );
    let prompts: Vec<_> = p_engine
        .read_projection(p_child)
        .await
        .unwrap()
        .session
        .messages
        .into_iter()
        .filter(|m| m.role == Role::User)
        .collect();
    assert_eq!(prompts.len(), 1);
    assert!(
        matches!(&prompts[0].parts[0], PartProjection::Text { text, .. } if text == "[mail from main] inspect parser\n\n[mail from main] also inspect tests")
    );
    p_engine
        .mail_send(
            p_root,
            MailEndpoint::Channel(dm.clone()),
            MailKind::Message,
            "parent followup".into(),
        )
        .await
        .unwrap();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            assistant_turns(&p_engine, p_child).await >= 2
        })
        .await
    );
    drop(p_root_turn);
    p_sup
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
}

// ---- Section 7: Handle naming (table-driven) ----

struct FixedRng(usize);
impl HandleRng for FixedRng {
    fn pick(&mut self, bound: usize) -> usize {
        self.0.min(bound - 1)
    }
}

struct NamingTeam {
    engine: Arc<SessionEngine>,
    sup: Arc<ResidentSupervisor>,
    root: SessionId,
}

async fn naming_team(rng: Option<Box<dyn HandleRng>>) -> NamingTeam {
    let base =
        support::engine_with_provider(Arc::new(StaticFake("ok")), PermissionRules::default()).await;
    let engine = Arc::new(match rng {
        Some(r) => base.with_handle_rng(r),
        None => base,
    });
    let root = support::create_root_session(&engine, "hya-main", "fake", empty_workdir()).await;
    let sup = ResidentSupervisor::start(engine.clone());
    let lead = support::agent_spec("hya-main", "fake", empty_workdir());
    ensure_main_agent(&sup, &engine, root, &lead).await;
    NamingTeam { engine, sup, root }
}

async fn spawn_named(
    t: &NamingTeam,
    parent: SessionId,
    agent_id: &str,
    subagent_type: Option<&str>,
) -> (SessionId, String) {
    let agent = support::agent_spec(agent_id, "fake", empty_workdir());
    let binding = t.engine.bind_runtime(&agent.workdir).unwrap();
    let resolved = (
        binding.clone(),
        Arc::from([]),
        binding.agent_resource_policy("hya-scout").unwrap(),
        None,
    );
    match subagent_type {
        Some(ty) => t
            .sup
            .spawn_resident_typed(
                parent,
                agent,
                resolved,
                "work".into(),
                TaskSpawnOrigin {
                    subagent_type: ty.into(),
                    ..Default::default()
                },
                None,
                None,
            )
            .await
            .unwrap(),
        None => t
            .sup
            .spawn_resident(parent, agent, resolved, "work".into(), None, None)
            .await
            .unwrap(),
    }
}

fn assert_named(handle: &str, parent: &str, prefix: &str) {
    let leaf = handle
        .strip_prefix(&format!("{parent}/"))
        .unwrap_or_else(|| panic!("{handle}"));
    assert!(!leaf.contains('/'));
    let name = leaf
        .strip_prefix(&format!("{prefix}-"))
        .unwrap_or_else(|| panic!("{handle}"));
    assert!(operator_names().contains(&name), "`{name}` from `{handle}`");
}

#[tokio::test]
async fn handle_naming_prefix_sanitization_nesting_and_seeded_reproducibility() {
    let team = naming_team(None).await;
    for (agent_id, subagent_type, expected_prefix) in [
        ("hya-scout", None, "hya-scout"),
        ("overlay", Some("scout"), "scout"),
        ("hya-scout", Some("Acme_Scout.v2"), "acme-scout-v2"),
        ("hya-scout", Some("__"), "agent"),
    ] {
        let (child, handle) = spawn_named(&team, team.root, agent_id, subagent_type).await;
        assert_named(&handle, "main", expected_prefix);
        let proj = team.engine.read_projection(team.root).await.unwrap();
        let t = team_projection(&proj);
        assert_eq!(t.roster[&handle].session, child);
        assert_eq!(
            t.resolve_dm("main", scope::leaf(&handle)),
            t.resolve_dm("main", &handle)
        );
    }
    let (dev, dev_handle) = spawn_named(&team, team.root, "hya-scout", Some("dev")).await;
    let (_, nested) = spawn_named(&team, dev, "hya-task", None).await;
    assert_named(&dev_handle, "main", "dev");
    assert_named(&nested, &dev_handle, "hya-task");

    let draw = |seed| async move {
        let seeded = naming_team(Some(Box::new(SplitMix64::seeded(seed)))).await;
        let mut out = Vec::new();
        for _ in 0..3 {
            out.push(spawn_named(&seeded, seeded.root, "hya-scout", None).await.1);
        }
        out
    };
    let first = draw(11).await;
    assert_eq!(first, draw(11).await);
    assert_eq!(first.iter().collect::<BTreeSet<_>>().len(), 3);
}

#[tokio::test]
async fn handle_naming_never_reuses_archived_leaf_and_preserves_legacy_counter_handles() {
    let team = naming_team(Some(Box::new(FixedRng(0)))).await;
    let (first, first_h) = spawn_named(&team, team.root, "hya-scout", Some("scout")).await;
    assert_eq!(first_h, format!("main/scout-{}", operator_names()[0]));
    team.sup
        .archive_member(team.root, &first_h, "done for now")
        .await
        .unwrap();
    let (second, second_h) = spawn_named(&team, team.root, "hya-scout", Some("scout")).await;
    assert_ne!(second, first);
    assert_eq!(
        second_h,
        format!("main/scout-{}-{}", operator_names()[0], operator_names()[1])
    );
    send_mail(&team.engine, team.root, &first_h, "resume").await;
    let proj = team.engine.read_projection(team.root).await.unwrap();
    let t = team_projection(&proj);
    assert_eq!(
        (t.roster[&first_h].session, t.roster[&second_h].session),
        (first, second)
    );

    // Legacy counter handle (`scout-1`) stays resolvable on replay without granting a DM.
    let legacy = naming_team(None).await;
    let old = create_child(&legacy.engine, legacy.root, "scout", "fake").await;
    legacy
        .engine
        .store()
        .append_event(
            legacy.root,
            &ExtensionEvent::AgentRegistered {
                session: legacy.root,
                agent_session: old,
                handle: "scout-1".into(),
                parent: Some("main".into()),
                agent_type: AgentName::new("scout"),
                mode: SubagentMode::Resident,
            }
            .into_event(),
        )
        .await
        .unwrap();
    let (_, new_h) = spawn_named(&legacy, legacy.root, "hya-scout", Some("scout")).await;
    assert_named(&new_h, "main", "scout");
    let replayed = hya_proto::Projection::from_events_with(
        &legacy.engine.replay(legacy.root).await.unwrap(),
        &projection_reducers(),
    );
    let replayed_team = team_projection(&replayed);
    assert_eq!(replayed_team.roster["main/scout-1"].session, old);
    assert!(replayed_team.roster.contains_key(&new_h));
    assert_eq!(replayed_team.resolve_dm("main", "scout-1"), None);
    let err = legacy
        .engine
        .mail_send(
            legacy.root,
            MailEndpoint::Handle("scout-1".into()),
            MailKind::Message,
            "still addressable".into(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no DM channel"), "{err}");
}

// ---- Section 8: Harness-allocated coordination tools on narrow bundle views ----

#[derive(Default)]
struct CoordScriptProvider {
    root: Mutex<Option<SessionId>>,
    requests: Mutex<Vec<(SessionId, CompletionRequest)>>,
    scout_started: Notify,
    mail_sent: Notify,
}

#[async_trait]
impl Provider for CoordScriptProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m, 200_000)
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push((s, req.clone()));
        let steps = if *self.root.lock().unwrap() == Some(s) {
            vec![
                FakeStep::Text("lead done".into()),
                FakeStep::Finish(FinishReason::Stop),
            ]
        } else {
            let calls: usize = req
                .messages
                .iter()
                .filter_map(|msg| match msg {
                    Message::Assistant { parts, .. } => Some(
                        parts
                            .iter()
                            .filter(|p| matches!(p, Part::Tool { .. }))
                            .count(),
                    ),
                    _ => None,
                })
                .sum();
            match calls {
                0 => {
                    self.scout_started.notify_one();
                    self.mail_sent.notified().await;
                    vec![
                        FakeStep::ToolCall {
                            name: "report".into(),
                            input: json!({"result": "early findings"}),
                        },
                        FakeStep::Finish(FinishReason::ToolCalls),
                    ]
                }
                1 => {
                    let txt = serde_json::to_string(&req.messages).unwrap();
                    let start = txt.rfind("channel://").unwrap();
                    let path: String = txt[start..]
                        .chars()
                        .take_while(|c| {
                            c.is_ascii_alphanumeric() || matches!(c, ':' | '/' | '-' | '?' | '=')
                        })
                        .collect();
                    vec![
                        FakeStep::ToolCall {
                            name: "read".into(),
                            input: json!({"path": path}),
                        },
                        FakeStep::Finish(FinishReason::ToolCalls),
                    ]
                }
                2 => vec![
                    FakeStep::ToolCall {
                        name: "report".into(),
                        input: json!({"result": "SCOUT_FINAL_REPORT"}),
                    },
                    FakeStep::Finish(FinishReason::ToolCalls),
                ],
                _ => vec![
                    FakeStep::Text("reported".into()),
                    FakeStep::Finish(FinishReason::Stop),
                ],
            }
        };
        Ok(stream_steps(&steps, s, m))
    }
}

#[tokio::test]
async fn narrow_bundle_member_gets_coordination_tools_reads_dm_and_reports() {
    const BUNDLE: &str = "schema_version: 2\nkind: Bundle\nidentity: { id: acme/coordination, version: 1.0.0, publisher: acme }\nagents:\n  - id: lead\n    role: main\n    can_spawn: [scout]\n    resource_view: { allow: [harness:tool/grep] }\n  - id: scout\n    role: subagent\n    resource_view: { allow: [harness:tool/grep, harness:tool/glob] }\n";
    let prepared = prepare_package(BundleSource::new(
        "coordination",
        vec![SourceFile::new("bundle.yaml", BUNDLE)],
    ))
    .unwrap();
    let catalog = Arc::new(
        AgentCatalog::new(Arc::new(
            BundleCatalog::from_prepared(prepared.bundles()).unwrap(),
        ))
        .unwrap(),
    );
    let provider = Arc::new(CoordScriptProvider::default());
    let (lifecycle, lifecycle_rx) = LifecyclePlane::new();
    let engine = Arc::new(
        support::engine_with_router_and_runtime(
            Arc::new(ProviderRouter::new().with(provider.clone())),
            support::builtin_test_runtime_with_catalog(catalog),
            PermissionRules::new(vec![Rule::new(Action::Read, "*", Mode::Allow)]),
        )
        .await
        .with_lifecycle(lifecycle)
        .with_governor(SubagentGovernor::new(SubagentLimits::default())),
    );
    let sup = ResidentSupervisor::start(engine.clone());
    tokio::spawn(run_lifecycle_service(
        engine.clone(),
        sup.clone(),
        lifecycle_rx,
        CancellationToken::new(),
    ));

    let root = support::create_root_session(&engine, "lead", "fake", empty_workdir()).await;
    *provider.root.lock().unwrap() = Some(root);
    let binding = engine.bind_runtime(empty_workdir()).unwrap();
    let lead_spec = support::agent_spec("lead", "fake", empty_workdir());
    sup.ensure_main(
        root,
        lead_spec.clone(),
        (
            binding.clone(),
            Arc::from([]),
            binding.agent_resource_policy("lead").unwrap(),
        ),
        None,
        None,
    )
    .await
    .unwrap();
    engine
        .admit_user_prompt(root, "coordinate".into())
        .await
        .unwrap();
    engine
        .run_turn_with_external_dirs_and_guidance(
            root,
            &lead_spec,
            CancellationToken::new(),
            &[],
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let (child, handle) = spawn_res(
        &sup,
        &engine,
        root,
        &support::agent_spec("scout", "fake", empty_workdir()),
        "find the marker",
        None,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), provider.scout_started.notified())
        .await
        .unwrap();
    send_mail(&engine, root, &handle, "status check: where is the marker?").await;
    provider.mail_sent.notify_one();

    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            team_projection(&engine.read_projection(root).await.unwrap())
                .archived
                .get(&handle)
                .is_some_and(|e| e.reason == ArchiveReason::Reported)
        })
        .await
    );
    let proj = engine.read_projection(root).await.unwrap();
    let team = team_projection(&proj);
    let dm = team
        .channels
        .iter()
        .find(|(_, ch)| ch.kind == ChannelKind::Dm && ch.members.contains(&handle))
        .unwrap()
        .0
        .clone();
    let gate = engine
        .replay(child)
        .await
        .unwrap()
        .into_iter()
        .find_map(|e| match e.event {
            Event::ToolError { message_text, .. } if message_text.contains("unread mail") => {
                Some(message_text)
            }
            _ => None,
        })
        .unwrap();
    assert!(gate.contains(&format!("#{dm}")) && gate.contains(&format!("read channel://{dm}")));
    assert!(
        team.inboxes["main"]
            .iter()
            .any(|i| team.mail[*i].body.contains("SCOUT_FINAL_REPORT"))
    );

    let tools_of = |r: &CompletionRequest| -> BTreeSet<String> {
        r.tools.iter().map(|s| s.name.as_str().into()).collect()
    };
    let reqs = provider.requests.lock().unwrap().clone();
    let lead_req = &reqs.iter().find(|(s, _)| *s == root).unwrap().1;
    let lead_tools = tools_of(lead_req);
    for t in [
        "grep",
        "wait",
        "task",
        "archive",
        "send",
        "list_channel",
        "read",
    ] {
        assert!(lead_tools.contains(t), "lead lacks `{t}`");
    }
    for absent in ["report", "bash", "glob"] {
        assert!(!lead_tools.contains(absent));
    }
    let lead_sys = lead_req.system.as_deref().unwrap_or_default();
    assert!(lead_sys.contains("NEVER call `report`") && lead_sys.contains("`task`"));

    let scout_req = &reqs.iter().find(|(s, _)| *s == child).unwrap().1;
    let scout_tools = tools_of(scout_req);
    for t in [
        "grep",
        "glob",
        "report",
        "wait",
        "send",
        "list_channel",
        "read",
    ] {
        assert!(scout_tools.contains(t), "scout lacks `{t}`");
    }
    for absent in ["task", "archive", "bash"] {
        assert!(!scout_tools.contains(absent));
    }
    let scout_sys = scout_req.system.as_deref().unwrap_or_default();
    assert!(
        scout_sys.contains("`report`")
            && !scout_sys.contains("`task`")
            && !scout_sys.contains("NEVER call `report`")
    );
}

// ---- Section 9: Isolated subprocess SQL counter test for stream deltas ----

struct DeltaSqlProvider(mpsc::UnboundedSender<u64>);
#[async_trait]
impl Provider for DeltaSqlProvider {
    fn id(&self) -> &str {
        "hya"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        DevProvider::new().capabilities(m)
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let (part, reasoning, call) = (PartId::new(), PartId::new(), ToolCallId::new());
        let mut events = vec![Event::TextStart {
            session,
            message,
            part,
        }];
        events.extend((0..32).map(|_| Event::TextDelta {
            session,
            message,
            part,
            delta: "x".into(),
        }));
        events.push(Event::TextEnd {
            session,
            message,
            part,
        });
        events.push(Event::ReasoningStart {
            reason: None,
            session,
            message,
            part: reasoning,
        });
        events.extend((0..32).map(|_| Event::ReasoningDelta {
            session,
            message,
            part: reasoning,
            delta: "x".into(),
        }));
        events.push(Event::ReasoningEnd {
            session,
            message,
            part: reasoning,
            provider_data: None,
        });
        events.extend((0..32).map(|_| Event::ToolInputDelta {
            session,
            message,
            part,
            call,
            name: "read".into(),
            delta: "x".into(),
        }));
        let (mut iter, counts, mut before) = (events.into_iter(), self.0.clone(), None);
        Ok(Box::pin(stream::poll_fn(move |_| {
            if let Some(prev) = before.take() {
                counts.send(bench::snapshot().statements - prev).unwrap();
            }
            let ev = iter.next();
            if matches!(
                ev,
                Some(
                    Event::TextDelta { .. }
                        | Event::ReasoningDelta { .. }
                        | Event::ToolInputDelta { .. }
                )
            ) {
                before = Some(bench::snapshot().statements);
            }
            std::task::Poll::Ready(ev.map(Ok))
        })))
    }
}

#[tokio::test]
async fn actor_stream_deltas_execute_no_sql() {
    support::run_isolated_subproc(
        "stream_claim_sql",
        "subagent_lifecycle::actor_stream_deltas_execute_no_sql",
        || async {
            let dir = support::TestDir::new("stream-claim-sql");
            let store = SessionStore::connect(dir.path().join("sessions.db").to_str().unwrap())
                .await
                .unwrap();
            bench::warm(&store).await.unwrap();
            let (counts, mut measured) = mpsc::unbounded_channel();
            let (perm, _) = PermissionPlane::new(PermissionRules::default());
            let engine = Arc::new(
                SessionEngine::new(
                    store,
                    Arc::new(ProviderRouter::new().with(Arc::new(DeltaSqlProvider(counts)))),
                    support::builtin_test_runtime().runtime,
                    perm,
                    EventBus::default(),
                )
                .with_round_retry_waits(&support::ZERO_RETRY_WAITS),
            );
            let agent = support::agent_spec("hya-task", "hya/offline", empty_workdir());
            let root =
                support::create_root_session(&engine, "hya-main", "hya/offline", &agent.workdir)
                    .await;
            let root_turn = engine.try_begin_turn(root).unwrap();
            let child = create_child(&engine, root, "hya-task", "hya/offline").await;
            let sup = ResidentSupervisor::start(engine.clone());
            sup.register_existing_resident(
                root,
                child,
                "worker-1".into(),
                agent,
                Some("stream".into()),
            )
            .await
            .unwrap();
            let mut sql = Vec::new();
            tokio::time::timeout(Duration::from_secs(5), async {
                for _ in 0..96 {
                    sql.push(measured.recv().await.unwrap());
                }
                while engine.turn_active(child) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            engine.begin_drain(FinishCause::Shutdown);
            drop(root_turn);
            sup.drain(FinishCause::Shutdown, Duration::from_secs(5))
                .await;
            assert_eq!(sql, vec![0; 96]);
            assert!(
                engine
                    .read_projection(child)
                    .await
                    .unwrap()
                    .session
                    .messages
                    .iter()
                    .any(|m| m.finish == Some(FinishReason::Stop))
            );
        },
    )
    .await;
}
