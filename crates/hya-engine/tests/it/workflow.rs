//! Integration tests for `hya-engine`: user-assembled workflow DAGs and deadline notices.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Condvar, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream};
use hya_bundle::AgentRole;
use hya_engine::hooks::{
    ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput, CommandExecuteBeforeOutcome,
    HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome, NoopHookHost,
    TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput, ToolExecuteAfterOutcome,
    ToolExecuteBeforeInput, ToolExecuteBeforeOutcome,
};
use hya_engine::{
    AgentCatalog, AgentSpec, CompiledWorkflow, CoreError, CreateSession, EventBus, FailurePolicy,
    PreparedWorkflowRun, ResidentSupervisor, SessionEngine, StageStatus, SubagentGovernor,
    SubagentLimits, TurnBinding, WorkflowError, WorkflowRunContext, WorkflowRunReport,
    WorkflowStatus, load_workflow_file, prepare_workflow_run, run_workflow,
};
use hya_proto::context::REASONING_DEADLINE_NOTICE_PREFIX;
use hya_proto::{
    AgentName, Envelope, Event, FinishCause, FinishReason, Message, MessageId, ModelRef, Part,
    PartId, SessionId, SessionKind, WorkflowMemberRole, WorkflowRunId, WorkflowStageStatus,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::{SessionStore, StoreError};
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, Tool};
use hya_workflow::{WorkflowSource, compile};
use serde_json::json;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

const TWO_STAGE_WORKFLOW: &str = "---\nkind: Workflow\nname: feature-flow\ndescription: Explore then build.\ninputs:\n  target: What to explore.\nnodes:\n  explore:\n    agent: explorer\n    directive: EXPLORE {{input.target}}\n  build:\n    agent: builder\n    directive: BUILD from direct evidence.\n---\nflowchart TD\n  explore --> build\n";

fn directive_of(text: &str) -> &str {
    text.trim_start_matches("[mail from main] ")
}

fn user_text_of(req: &CompletionRequest) -> String {
    req.messages
        .iter()
        .filter_map(|m| match m {
            Message::User { parts, .. } => Some(parts.iter().filter_map(|p| match p {
                Part::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })),
            _ => None,
        })
        .flatten()
        .collect()
}

fn default_caps() -> Capabilities {
    Capabilities {
        streaming_tool_calls: true,
        usage_reporting: true,
        max_context: 200_000,
        ..Capabilities::default()
    }
}

fn stream_steps(steps: &[FakeStep], session: SessionId) -> Result<EventStream, ProviderError> {
    let evs = FakeProvider::materialize(steps, session, MessageId::new());
    Ok(Box::pin(stream::iter(evs.into_iter().map(Ok))))
}

fn compile_wf(name: &str, source: &str) -> CompiledWorkflow {
    compile(WorkflowSource::new(name, source)).unwrap()
}

fn builtin_tools() -> Arc<hya_tool::ToolRegistry> {
    static TOOLS: OnceLock<Arc<hya_tool::ToolRegistry>> = OnceLock::new();
    TOOLS
        .get_or_init(|| Arc::new(hya_tool_loader::builtins()))
        .clone()
}

fn default_catalog() -> Arc<AgentCatalog> {
    static CATALOG: OnceLock<Arc<AgentCatalog>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            support::test_catalog(&[
                ("explorer", AgentRole::Subagent, &[]),
                ("builder", AgentRole::Subagent, &[]),
                ("reviewer", AgentRole::Subagent, &[]),
                ("planner", AgentRole::Subagent, &[]),
            ])
        })
        .clone()
}

fn base_spec() -> AgentSpec {
    support::agent_spec("hya-main", "fake", "/tmp")
}

struct WfEnv {
    eng: Arc<SessionEngine>,
    lead: SessionId,
    sup: Arc<ResidentSupervisor>,
    binding: TurnBinding,
    base: AgentSpec,
    events: broadcast::Receiver<Envelope>,
}

impl WfEnv {
    async fn new(provider: Arc<dyn Provider>) -> Self {
        Self::custom(
            provider,
            default_catalog(),
            SubagentLimits::default(),
            None,
            None,
            Path::new("/tmp"),
        )
        .await
    }

    async fn with_budget(provider: Arc<dyn Provider>, budget: u64) -> Self {
        let limits = SubagentLimits {
            per_run_budget: budget,
            ..SubagentLimits::default()
        };
        Self::custom(
            provider,
            default_catalog(),
            limits,
            None,
            None,
            Path::new("/tmp"),
        )
        .await
    }

    async fn custom(
        provider: Arc<dyn Provider>,
        catalog: Arc<AgentCatalog>,
        limits: SubagentLimits,
        hooks: Option<Arc<dyn HookDispatcher>>,
        sidecar: Option<Arc<dyn hya_engine::sidecar::SidecarEnvironment>>,
        workdir: &Path,
    ) -> Self {
        let router = Arc::new(ProviderRouter::new().with(provider));
        let runtime = support::test_runtime_with_catalog(builtin_tools(), catalog);
        let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
        let store = SessionStore::connect_memory().await.unwrap();
        let bus = EventBus::default();
        let events = bus.subscribe();
        let mut eng = SessionEngine::new(store, router, runtime, perm, bus)
            .with_round_retry_waits(&support::ZERO_RETRY_WAITS)
            .with_governor(SubagentGovernor::new(limits));
        if let Some(h) = hooks {
            eng = eng.with_hooks(h);
        }
        if let Some(s) = sidecar {
            eng = eng.with_sidecar_environment(s);
        }
        let eng = Arc::new(eng);
        let lead = support::create_root_session(&eng, "hya-main", "fake", Path::new("/tmp")).await;
        let sup = ResidentSupervisor::start(eng.clone());
        let binding = eng.bind_runtime(workdir).unwrap();
        let base = eng
            .agent_spec_for_binding(&binding, &base_spec(), "hya-main")
            .unwrap();
        Self {
            eng,
            lead,
            sup,
            binding,
            base,
            events,
        }
    }

    fn ctx(&self, inputs: BTreeMap<String, String>) -> WorkflowRunContext {
        WorkflowRunContext {
            binding: self.binding.clone(),
            caller: "hya-main".into(),
            base_agent: self.base.clone(),
            inputs,
            resident_supervisor: Some(self.sup.clone()),
            routing: None,
        }
    }

    async fn run(&self, def: &CompiledWorkflow) -> Result<WorkflowRunReport, WorkflowError> {
        self.run_with(def, BTreeMap::new(), CancellationToken::new())
            .await
    }

    async fn run_with(
        &self,
        def: &CompiledWorkflow,
        inputs: BTreeMap<String, String>,
        cancel: CancellationToken,
    ) -> Result<WorkflowRunReport, WorkflowError> {
        run_workflow(self.eng.clone(), self.lead, def, self.ctx(inputs), cancel).await
    }

    async fn prepare(
        &self,
        def: &CompiledWorkflow,
        run_id: Option<WorkflowRunId>,
    ) -> Result<PreparedWorkflowRun, WorkflowError> {
        prepare_workflow_run(
            self.eng.clone(),
            self.lead,
            def,
            self.ctx(BTreeMap::new()),
            run_id,
        )
        .await
    }

    async fn spawn_parked(&self, label: &str) -> (SessionId, String) {
        let (agent, runtime) = self
            .eng
            .resolve_resident_runtime(self.binding.clone(), &self.base, &AgentName::new("builder"))
            .unwrap();
        let (sid, handle, _) = self
            .sup
            .spawn_resident_parked(self.lead, agent, runtime, label.into(), None, None)
            .await
            .unwrap();
        (sid, handle)
    }

    async fn live_count(&self) -> u64 {
        self.eng.store().live_member_count(self.lead).await.unwrap()
    }

    async fn has_completed_stage_event(&self) -> bool {
        self.eng.replay(self.lead).await.unwrap().iter().any(|e| {
            matches!(
                e.event,
                Event::WorkflowStageFinished {
                    status: WorkflowStageStatus::Completed,
                    ..
                }
            )
        })
    }

    async fn assert_archived_resident(&self, agent_type: &str) {
        let proj = self.eng.read_projection(self.lead).await.unwrap();
        let archived = proj
            .team
            .archived
            .values()
            .find(|e| e.agent_type.as_str() == agent_type)
            .expect("resident remains in archive");
        assert!(!proj.team.roster.contains_key(&archived.handle));
        assert!(
            !self
                .eng
                .store()
                .active_actor_ids()
                .await
                .unwrap()
                .contains(&archived.session)
        );
    }

    async fn drain(&self) {
        self.sup
            .drain(FinishCause::Shutdown, hya_engine::DRAIN_DEADLINE)
            .await;
    }
}

fn stage_status(report: &WorkflowRunReport, name: &str) -> String {
    report
        .stages
        .iter()
        .find(|s| s.stage == name)
        .unwrap()
        .status
        .to_string()
}

/// Multi-mode test provider covering recording, scripted verdicts, barrier overlap,
/// hang gates for cancellation, and single-turn tool probes.
struct RecordingProvider {
    capacity: Mutex<Option<(SessionStore, SessionId)>>,
    live_counts: Mutex<Vec<(bool, u64)>>,
    systems: Mutex<Vec<(SessionId, Option<String>)>>,
    requests: Mutex<Vec<(SessionId, String)>>,
    models: Mutex<Vec<ModelRef>>,
    verdicts: Mutex<Vec<bool>>,
    hang_verifier: AtomicBool,
    hang_all: AtomicBool,
    started: Notify,
    barrier: Option<tokio::sync::Barrier>,
    entered: AtomicUsize,
    probe_tool: Option<&'static str>,
    probe_turns: Mutex<BTreeMap<String, u32>>,
}

impl RecordingProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            capacity: Mutex::new(None),
            live_counts: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
            models: Mutex::new(Vec::new()),
            verdicts: Mutex::new(Vec::new()),
            hang_verifier: AtomicBool::new(false),
            hang_all: AtomicBool::new(false),
            systems: Mutex::new(Vec::new()),
            started: Notify::new(),
            barrier: None,
            entered: AtomicUsize::new(0),
            probe_tool: None,
            probe_turns: Mutex::new(BTreeMap::new()),
        })
    }

    fn with_barrier(parties: usize) -> Arc<Self> {
        let mut p = Arc::try_unwrap(Self::new()).ok().unwrap();
        p.barrier = Some(tokio::sync::Barrier::new(parties));
        Arc::new(p)
    }

    fn with_probe(tool: &'static str) -> Arc<Self> {
        let mut p = Arc::try_unwrap(Self::new()).ok().unwrap();
        p.probe_tool = Some(tool);
        Arc::new(p)
    }

    fn script_verdict(&self, met: bool) -> &Self {
        self.verdicts.lock().unwrap().push(met);
        self
    }

    async fn wait_started(&self) {
        tokio::time::timeout(Duration::from_secs(2), self.started.notified())
            .await
            .expect("provider request starts");
    }

    fn prompts(&self) -> Vec<(SessionId, String)> {
        self.requests.lock().unwrap().clone()
    }

    fn complete(
        &self,
        req: &CompletionRequest,
        session: SessionId,
        user_text: String,
        reply: String,
    ) -> Result<EventStream, ProviderError> {
        self.models
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(req.model.clone());
        self.systems
            .lock()
            .unwrap()
            .push((session, req.system.clone()));
        self.requests.lock().unwrap().push((session, user_text));
        stream_steps(
            &[FakeStep::Text(reply), FakeStep::Finish(FinishReason::Stop)],
            session,
        )
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(default_caps())
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        _msg: MessageId,
    ) -> Result<EventStream, ProviderError> {
        if self.hang_all.load(Ordering::SeqCst) {
            self.started.notify_one();
            return futures::future::pending().await;
        }
        let user_text = user_text_of(&req);
        if let Some(barrier) = &self.barrier {
            if directive_of(&user_text).starts_with("PARALLEL") {
                self.entered.fetch_add(1, Ordering::SeqCst);
                barrier.wait().await;
            }
            return stream_steps(
                &[
                    FakeStep::Text("OVERLAPPED".into()),
                    FakeStep::Finish(FinishReason::Stop),
                ],
                session,
            );
        }
        if let Some(probe) = self.probe_tool {
            if user_text.contains("independent verifier") {
                return stream_steps(
                    &[
                        FakeStep::Text(r#"{"met": true, "reason": "scripted"}"#.into()),
                        FakeStep::Finish(FinishReason::Stop),
                    ],
                    session,
                );
            }
            let turn = {
                let mut map = self.probe_turns.lock().unwrap();
                let c = map.entry(session.to_string()).or_insert(0);
                *c += 1;
                *c
            };
            return if turn == 1 {
                stream_steps(
                    &[
                        FakeStep::ToolCall {
                            name: probe.into(),
                            input: json!({}),
                        },
                        FakeStep::Finish(FinishReason::ToolCalls),
                    ],
                    session,
                )
            } else {
                stream_steps(
                    &[
                        FakeStep::Text("PROBE_DONE".into()),
                        FakeStep::Finish(FinishReason::Stop),
                    ],
                    session,
                )
            };
        }
        assert!(
            req.tools.iter().any(|t| t.name.as_str() == "report"),
            "T2: every worker/verifier can report"
        );
        let cap = self.capacity.lock().unwrap().clone();
        if let Some((store, root)) = cap {
            let live = store.live_member_count(root).await.unwrap();
            self.live_counts
                .lock()
                .unwrap()
                .push((user_text.contains("independent verifier"), live));
        }
        let reply = if directive_of(&user_text).starts_with("EXPLORE") {
            "EXPLORER_REPORT"
        } else if user_text.contains("FAIL_ME") {
            return Err(ProviderError::Incompatible(
                "member scripted failure".into(),
            ));
        } else if user_text.contains("independent verifier") {
            if self.hang_verifier.load(Ordering::SeqCst) {
                self.started.notify_one();
                futures::future::pending::<()>().await;
                unreachable!();
            }
            let mut vs = self.verdicts.lock().unwrap();
            let met = if vs.is_empty() { true } else { vs.remove(0) };
            return self.complete(
                &req,
                session,
                user_text,
                format!("{{\"met\": {met}, \"reason\": \"scripted\"}}"),
            );
        } else if user_text.contains("FAIL_LOOP_RESUME") {
            return Err(ProviderError::Incompatible(
                "resumed loop worker scripted failure".into(),
            ));
        } else {
            "WORKER_DONE"
        };
        self.complete(&req, session, user_text, reply.to_string())
    }
}

fn tool_results_for(
    rx: &mut broadcast::Receiver<Envelope>,
    session: SessionId,
) -> Vec<Result<String, String>> {
    let mut out = Vec::new();
    while let Ok(env) = rx.try_recv() {
        match env.event {
            Event::ToolResult {
                session: s, output, ..
            } if s == session => out.push(Ok(output.to_string())),
            Event::ToolError {
                session: s,
                message_text,
                ..
            } if s == session => out.push(Err(message_text)),
            _ => {}
        }
    }
    out
}

#[derive(Debug)]
struct SidecarPingTool;

#[async_trait]
impl Tool for SidecarPingTool {
    fn name(&self) -> &str {
        "sidecar_ping"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        hya_proto::ToolSchema {
            name: hya_proto::ToolName::new("sidecar_ping"),
            description: "probe sidecar presence".into(),
            input_schema: json!({"type": "object", "properties": {}}),
            output_schema: None,
        }
    }

    async fn execute(
        &self,
        _: &hya_tool::ToolCtx,
        _: serde_json::Value,
    ) -> Result<serde_json::Value, hya_tool::ToolError> {
        Ok(json!({ "title": "ping", "output": "PONG_FROM_SIDECAR" }))
    }
}

fn sidecar_probe_catalog() -> Arc<AgentCatalog> {
    let bundles: Vec<_> = [("builder", true), ("reviewer", false)]
        .into_iter()
        .map(|(id, has_tool)| hya_bundle::Bundle {
            check: None,
            format_version: 4,
            identity: hya_bundle::BundleIdentity {
                id: format!("hya/test-{id}"),
                version: "0.0.0".into(),
                publisher: "hya-tests".into(),
            },
            namespace: None,
            digest: format!("test-only-{id}"),
            agents: vec![hya_bundle::PreparedAgent {
                private: true,
                id: AgentName::new(id),
                description: None,
                role: AgentRole::Subagent,
                color: None,
                prompt: Some(format!("{id} prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: hya_bundle::ModelPolicy::default(),
                workdir: None,
                resource_view: hya_bundle::ResourceView::default(),
                can_spawn: vec![],
                hook_refs: vec![],
                advertise: vec![],
                handoff: hya_bundle::HandoffPolicy::Model,
            }],
            tools: if has_tool {
                vec![hya_bundle::PreparedResource {
                    local_id: "sidecar_ping".into(),
                    stable_id: "sidecar_ping".into(),
                    source_path: "tools/sidecar_ping.ts".into(),
                    digest: "test-only-sidecar-ping".into(),
                    content: "// bound sidecar".into(),
                    binary_base64: None,
                    aliases: vec![],
                }]
            } else {
                vec![]
            },
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
            permission_modes: vec![],
        })
        .collect();
    let bc = hya_bundle::BundleCatalog::from_prepared(&bundles).unwrap();
    Arc::new(AgentCatalog::new(Arc::new(bc)).unwrap())
}

struct OnlyBuilderSidecarEnv(Mutex<Vec<String>>);

impl hya_engine::sidecar::SidecarEnvironment for OnlyBuilderSidecarEnv {
    fn factory_for(
        &self,
        _: &TurnBinding,
        stable_id: &str,
    ) -> Result<Option<Arc<dyn hya_engine::sidecar::BoundSidecarFactory>>, CoreError> {
        self.0.lock().unwrap().push(stable_id.into());
        Ok((stable_id == "builder").then(|| Arc::new(MarkerSidecar) as _))
    }
}

struct MarkerSidecar;

#[async_trait]
impl hya_engine::sidecar::BoundSidecarFactory for MarkerSidecar {
    async fn start(
        &self,
        _: hya_engine::sidecar::SidecarStart,
    ) -> Result<Box<dyn hya_engine::sidecar::SidecarHandle>, CoreError> {
        Ok(Box::new(MarkerSidecar))
    }
}

#[async_trait]
impl hya_engine::sidecar::SidecarHandle for MarkerSidecar {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
    fn tool_bindings(&self) -> Arc<[hya_tool::ResolvedTool]> {
        Arc::from([hya_tool::ResolvedTool {
            tool: Arc::new(SidecarPingTool),
            permission: hya_tool::ToolPermission::ReadOnly,
        }])
    }
}

struct StageLinkGate {
    role: WorkflowMemberRole,
    iteration: u32,
    entered: Notify,
    released: Mutex<bool>,
    release: Condvar,
}

impl StageLinkGate {
    fn new(role: WorkflowMemberRole, iteration: u32) -> Arc<Self> {
        Arc::new(Self {
            role,
            iteration,
            entered: Notify::new(),
            released: Mutex::new(false),
            release: Condvar::new(),
        })
    }
    async fn wait_entered(&self) {
        tokio::time::timeout(Duration::from_secs(3), self.entered.notified())
            .await
            .unwrap();
    }
    fn unblock(&self) {
        *self.released.lock().unwrap() = true;
        self.release.notify_one();
    }
}

struct RegistrationGate(Vec<Arc<StageLinkGate>>);

#[async_trait]
impl HookDispatcher for RegistrationGate {
    fn dispatch_event(&self, envelope: &Envelope) {
        if let Event::WorkflowStageMemberLinked {
            role, iteration, ..
        } = &envelope.event
        {
            for gate in &self.0 {
                if *role == gate.role && *iteration == gate.iteration {
                    gate.entered.notify_one();
                    tokio::task::block_in_place(|| {
                        let g = gate.released.lock().unwrap();
                        let (_guard, t) = gate
                            .release
                            .wait_timeout_while(g, Duration::from_secs(5), |r| !*r)
                            .unwrap();
                        assert!(!t.timed_out(), "link barrier not released");
                    });
                }
            }
        }
    }
    async fn command_execute_before(
        &self,
        i: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        NoopHookHost.command_execute_before(i).await
    }
    async fn text_complete(&self, i: TextCompleteInput) -> TextCompleteOutcome {
        NoopHookHost.text_complete(i).await
    }
    async fn message_user_before(&self, i: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        NoopHookHost.message_user_before(i).await
    }
    async fn chat_params(&self, i: ChatParamsInput) -> ChatParamsOutcome {
        NoopHookHost.chat_params(i).await
    }
    async fn tool_execute_before(&self, i: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        NoopHookHost.tool_execute_before(i).await
    }
    async fn tool_execute_after(&self, i: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        NoopHookHost.tool_execute_after(i).await
    }
}

struct NoticeProvider {
    inner: FakeProvider,
    think_first: bool,
    report_offered: Mutex<BTreeMap<SessionId, bool>>,
    last_user: Mutex<Vec<(SessionId, String)>>,
}

#[async_trait]
impl Provider for NoticeProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.inner.capabilities(model)
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let first = self
            .report_offered
            .lock()
            .unwrap()
            .insert(
                session,
                req.tools.iter().any(|t| t.name.as_str() == "report"),
            )
            .is_none();
        let last = req.messages.iter().rev().find_map(|m| match m {
            Message::User { parts, .. } => Some(
                parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        });
        self.last_user
            .lock()
            .unwrap()
            .push((session, last.unwrap_or_default()));
        if self.think_first && first {
            let part = PartId::new();
            let evs = [
                Event::ReasoningStart {
                    session,
                    message,
                    part,
                    reason: None,
                },
                Event::ReasoningDelta {
                    session,
                    message,
                    part,
                    delta: "weighing the options".into(),
                },
            ];
            return Ok(stream::iter(evs.map(Ok::<_, ProviderError>))
                .chain(stream::pending())
                .boxed());
        }
        self.inner.stream(req, session, message).await
    }
}

// ---------------------------------------------------------------------------
// 1. DAG ordering, handoff, fan-out/fan-in, and parallel overlap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stage_ordering_evidence_handoff_and_fan_out_join() {
    let workdir = support::TestDir::new("workflow-two-stage");
    let wf_dir = workdir.path().join(".hya/workflows");
    std::fs::create_dir_all(&wf_dir).unwrap();
    let path: PathBuf = wf_dir.join("feature.hya.md");
    std::fs::write(&path, TWO_STAGE_WORKFLOW).unwrap();
    let def = load_workflow_file(&path).unwrap();

    let p = RecordingProvider::new();
    let env = WfEnv::custom(
        p.clone(),
        default_catalog(),
        SubagentLimits::default(),
        None,
        None,
        workdir.path(),
    )
    .await;
    let report = env
        .run_with(
            &def,
            BTreeMap::from([("target".into(), "the retry paths".into())]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(report.status, WorkflowStatus::Completed);
    assert_eq!(report.stages.len(), 2);
    assert_eq!(
        (
            report.stages[0].stage.as_str(),
            report.stages[0].status.to_string(),
            report.stages[1].stage.as_str()
        ),
        ("explore", "done".into(), "build")
    );
    let prompts = p.prompts();
    assert_eq!(prompts.len(), 2);
    assert!(directive_of(&prompts[0].1).starts_with("EXPLORE the retry paths"));
    assert!(prompts[1].1.contains("BUILD from") && prompts[1].1.contains("EXPLORER_REPORT"));
    assert_ne!(prompts[0].0, prompts[1].0);
    assert_ne!(prompts[0].0, env.lead);

    let fan_def = compile_wf(
        "fan.hya.md",
        "---\nkind: Workflow\nname: fan-flow\ndescription: Fan out then review.\nnodes:\n  plan:\n    agent: planner\n    directive: PLAN\n  impl_a:\n    agent: builder\n    directive: IMPL A\n  impl_b:\n    agent: builder\n    directive: IMPL B\n  review:\n    agent: reviewer\n    directive: REVIEW both\n---\nflowchart TD\n  plan --> impl_a & impl_b\n  impl_a & impl_b --> review\n",
    );
    let fp = RecordingProvider::new();
    let fenv = WfEnv::new(fp.clone()).await;
    let fr = fenv.run(&fan_def).await.unwrap();
    assert_eq!((fr.status, fr.stages.len()), (WorkflowStatus::Completed, 4));
    let fprompts = fp.prompts();
    assert_eq!(
        fprompts
            .iter()
            .filter(|(_, t)| t.contains("IMPL A"))
            .count(),
        1
    );
    let (_, rev) = fprompts
        .iter()
        .find(|(_, t)| directive_of(t).starts_with("REVIEW both"))
        .unwrap();
    assert!(rev.contains("<stage id=\"impl_a\"") && rev.contains("<stage id=\"impl_b\""));
}

#[tokio::test]
async fn same_level_anonymous_stages_overlap_provider_execution() {
    let wf = compile_wf(
        "overlap.hya.md",
        "---\nkind: Workflow\nname: overlap-flow\ndescription: Two independent Stages overlap.\nnodes:\n  left:\n    agent: builder\n    directive: PARALLEL left\n  right:\n    agent: builder\n    directive: PARALLEL right\n---\nflowchart TD\n  left\n  right\n",
    );
    let p = RecordingProvider::with_barrier(2);
    let env = WfEnv::new(p.clone()).await;
    let report = tokio::time::timeout(Duration::from_secs(2), env.run(&wf))
        .await
        .expect("barrier reached")
        .unwrap();
    assert_eq!(report.status, WorkflowStatus::Completed);
    assert_eq!(p.entered.load(Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------------------
// 2. Failure policies: fail_fast, collect_all join skip, resumed loop failure
// ---------------------------------------------------------------------------

#[tokio::test]
async fn failure_policies_fail_fast_collect_all_and_resumed_loop_failure() {
    // fail_fast skips downstream levels while retaining every Stage row.
    let ff_def = compile_wf(
        "abort.hya.md",
        "---\nkind: Workflow\nname: abort-flow\ndescription: Fail-fast.\non_failure: fail_fast\nnodes:\n  broken:\n    agent: builder\n    directive: FAIL_ME explode\n  after:\n    agent: reviewer\n    directive: Should never run.\n---\nflowchart TD\n  broken --> after\n",
    );
    let ffp = RecordingProvider::new();
    let ff = WfEnv::new(ffp.clone()).await.run(&ff_def).await.unwrap();
    assert_eq!((ff.status, ff.stages.len()), (WorkflowStatus::Failed, 2));
    assert_eq!(
        (ff.stages[0].stage.as_str(), ff.stages[0].status.to_string()),
        ("broken", "failed".into())
    );
    assert_eq!(ff.stages[1].status, StageStatus::Skipped);
    assert!(ffp.prompts().is_empty());

    // collect_all finishes independent branches while skipping failed joins.
    let ca_def = compile_wf(
        "tolerant.hya.md",
        "---\nkind: Workflow\nname: tolerant-flow\ndescription: Collect all.\non_failure: collect_all\nnodes:\n  good:\n    agent: builder\n    directive: IMPL GOOD\n  bad:\n    agent: builder\n    directive: FAIL_ME IMPL BAD\n  review:\n    agent: reviewer\n    directive: REVIEW all\n---\nflowchart TD\n  good & bad --> review\n",
    );
    assert_eq!(ca_def.definition().on_failure(), FailurePolicy::CollectAll);
    let cap = RecordingProvider::new();
    let ca_env = WfEnv::new(cap.clone()).await;
    let ca = tokio::time::timeout(Duration::from_secs(5), ca_env.run(&ca_def))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            ca.status,
            stage_status(&ca, "bad"),
            stage_status(&ca, "review")
        ),
        (WorkflowStatus::Failed, "failed".into(), "skipped".into())
    );
    assert!(
        !cap.prompts()
            .iter()
            .any(|(_, t)| directive_of(t).starts_with("REVIEW all"))
    );

    // collect_all applies to failures on a resumed loop worker turn.
    let lp_def = compile_wf(
        "tolerant-loop.hya.md",
        "---\nkind: Workflow\nname: tolerant-loop\ndescription: Later loop failure blocks review.\non_failure: collect_all\nnodes:\n  build:\n    agent: builder\n    directive: BUILD first round\n    mode: loop\n    verify:\n      agent: reviewer\n      until: FAIL_LOOP_RESUME\n      max_iterations: 3\n  review:\n    agent: reviewer\n    directive: REVIEW loop\n---\nflowchart TD\n  build --> review\n",
    );
    let lpp = RecordingProvider::new();
    lpp.script_verdict(false);
    let lp = WfEnv::new(lpp.clone())
        .await
        .run(&lp_def)
        .await
        .expect("collect_all retains loop failure");
    assert_eq!(
        (
            lp.status,
            stage_status(&lp, "build"),
            stage_status(&lp, "review")
        ),
        (WorkflowStatus::Failed, "failed".into(), "skipped".into())
    );
    assert!(
        !lpp.prompts()
            .iter()
            .any(|(_, t)| directive_of(t).starts_with("REVIEW loop"))
    );
}

// ---------------------------------------------------------------------------
// 3. Model routing & preflight validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn model_routing_and_preflight_validation_reject_invalid_runs_up_front() {
    let old_err = compile(WorkflowSource::new(
        "old.yaml",
        "\nname: old-flow\ndescription: Removed.\nstages:\n  - id: run\n    agent: builder\n    prompt: Run.\n",
    ))
    .unwrap_err();
    assert!(
        old_err
            .message()
            .contains("must start with YAML frontmatter"),
        "{old_err}"
    );

    // Remembered agent model defaults apply to unassigned stages while explicit routes win.
    let route_def = compile_wf(
        "remembered.hya.md",
        "---\nkind: Workflow\nname: remembered-stage-models\ndescription: Model routing.\nnodes:\n  explore:\n    agent: explorer\n    directive: EXPLORE remembered model.\n  build:\n    agent: builder\n    directive: BUILD explicit model.\n    model:\n      id: fake/explicit\n---\nflowchart TD\n  explore --> build\n",
    );
    let rp = RecordingProvider::new();
    let renv = WfEnv::new(rp.clone()).await;
    renv.eng
        .runtime_registry()
        .publish_agent_model_preferences(BTreeMap::from([
            ("explorer".into(), ModelRef::new("fake/remembered-explorer")),
            ("builder".into(), ModelRef::new("fake/ignored-builder")),
        ]));
    let refreshed = renv.eng.bind_runtime(Path::new("/tmp")).unwrap();
    let rctx = WorkflowRunContext {
        binding: refreshed,
        ..renv.ctx(BTreeMap::new())
    };
    assert_eq!(
        run_workflow(
            renv.eng.clone(),
            renv.lead,
            &route_def,
            rctx,
            CancellationToken::new()
        )
        .await
        .unwrap()
        .status,
        WorkflowStatus::Completed
    );
    assert_eq!(
        *rp.models.lock().unwrap(),
        vec![
            ModelRef::new("fake/remembered-explorer"),
            ModelRef::new("fake/explicit")
        ]
    );

    // Preflight rejections: missing input, unknown agent, budget overflow, missing supervisor.
    let pp = RecordingProvider::new();
    let penv = WfEnv::with_budget(pp.clone(), 2).await;
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "input.hya.md",
            "---\nkind: Workflow\nname: input-flow\ndescription: Require input.\ninputs:\n  target: Required.\nnodes:\n  run:\n    agent: builder\n    directive: Do {{input.target}}.\n---\nflowchart TD\n  run\n",
            &["`target` was not provided"],
        ),
        (
            "ghost.hya.md",
            "---\nkind: Workflow\nname: ghost-flow\ndescription: Unknown agent.\nnodes:\n  run:\n    agent: ghost-agent-not-installed\n    directive: Run.\n---\nflowchart TD\n  run\n",
            &["not spawnable by `hya-main`", "ghost-agent-not-installed"],
        ),
        (
            "wide.hya.md",
            "---\nkind: Workflow\nname: wide-flow\ndescription: Exceeds budget.\nnodes:\n  one:\n    agent: builder\n    directive: One.\n  two:\n    agent: builder\n    directive: Two.\n  three:\n    agent: reviewer\n    directive: Three.\n---\nflowchart TD\n  one & two --> three\n",
            &["exceed the per-run budget"],
        ),
    ];
    for (name, src, needles) in cases {
        let msg = penv
            .run(&compile_wf(name, src))
            .await
            .expect_err("must fail preflight")
            .to_string();
        for n in *needles {
            assert!(msg.contains(n), "{name}: expected `{n}` in `{msg}`");
        }
        assert!(pp.prompts().is_empty());
    }

    let anon = compile_wf(
        "anonymous.hya.md",
        "---\nkind: Workflow\nname: anonymous\ndescription: Supervisor preflight\nnodes:\n  work:\n    agent: builder\n    directive: Work.\n---\nflowchart TD\n  work\n",
    );
    let before = penv.eng.replay(penv.lead).await.unwrap().len();
    let no_sup = WorkflowRunContext {
        resident_supervisor: None,
        ..penv.ctx(BTreeMap::new())
    };
    let err = prepare_workflow_run(penv.eng.clone(), penv.lead, &anon, no_sup, None).await;
    assert!(
        matches!(err, Err(WorkflowError::Invalid { detail, .. }) if detail.contains("supervisor"))
    );
    assert_eq!(penv.eng.replay(penv.lead).await.unwrap().len(), before);
}

#[tokio::test]
async fn preflight_capacity_counts_parked_members_and_returns_typed_occupancy_refusal() {
    let one = compile_wf(
        "capacity.hya.md",
        "---\nkind: Workflow\nname: capacity-flow\ndescription: One slot.\nnodes:\n  run:\n    agent: builder\n    directive: Run.\n---\nflowchart TD\n  run\n",
    );
    let p = RecordingProvider::new();
    let env = WfEnv::with_budget(p.clone(), 1).await;
    let root_turn = env.eng.try_begin_turn(env.lead).unwrap();
    let first = env.prepare(&one, None).await.unwrap();
    let second = env.prepare(&one, None).await.unwrap();
    assert_eq!(env.eng.store().list_sessions().await.unwrap().len(), 1);
    let child = env
        .eng
        .create(CreateSession {
            parent: Some(env.lead),
            agent: AgentName::new("builder"),
            model: env.base.model.clone(),
            workdir: env.base.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: SessionKind::Project,
        })
        .await
        .unwrap();
    let agent = env
        .eng
        .agent_spec_for_binding(&env.binding, &env.base, "builder")
        .unwrap();
    env.sup
        .register_existing_resident(env.lead, child, "builder-parked".into(), agent, None)
        .await
        .unwrap();
    assert!(env.prepare(&one, None).await.is_err());
    assert!(p.prompts().is_empty());
    drop((first, second, root_turn));
    env.drain().await;

    let two = compile_wf(
        "occupancy.hya.md",
        &TWO_STAGE_WORKFLOW
            .replace("inputs:\n  target: What to explore.\n", "")
            .replace("{{input.target}}", "target"),
    );
    let env2 = WfEnv::with_budget(RecordingProvider::new(), 2).await;
    let rt2 = env2.eng.try_begin_turn(env2.lead).unwrap();
    let (_, handle) = env2.spawn_parked("Occupant").await;
    let res = env2.prepare(&two, None).await;
    assert!(matches!(
        res,
        Err(WorkflowError::Engine(CoreError::Store(
            StoreError::CapacityExceeded { .. }
        )))
    ));
    env2.sup.end_member(env2.lead, &handle).await.unwrap();
    drop(rt2);
    env2.drain().await;
}

// ---------------------------------------------------------------------------
// 4. Loop verifier stop authority and cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn loop_stage_verifier_owns_the_stop_decision() {
    let def = compile_wf(
        "loop.hya.md",
        "---\nkind: Workflow\nname: loop-flow\ndescription: Verify-gated build.\nnodes:\n  build:\n    agent: builder\n    directive: LOOP_BUILD the thing\n    mode: loop\n    verify:\n      agent: reviewer\n      until: Tests pass.\n      max_iterations: 4\n---\nflowchart TD\n  build\n",
    );
    let p = RecordingProvider::new();
    p.script_verdict(false).script_verdict(true);
    let env = WfEnv::new(p.clone()).await;
    *p.capacity.lock().unwrap() = Some((env.eng.store().clone(), env.lead));

    let report = env.run(&def).await.unwrap();
    assert_eq!(
        (
            report.status,
            report.stages.len(),
            report.stages[0].status.to_string()
        ),
        (WorkflowStatus::Completed, 1, "done".into())
    );
    assert!(
        report.stages[0].output.contains("WORKER_DONE")
            && report.stages[0].output.contains("[verified: scripted]")
    );

    let prompts = p.prompts();
    let worker_rounds: Vec<_> = prompts
        .iter()
        .filter(|(_, t)| directive_of(t).starts_with("LOOP_BUILD"))
        .collect();
    assert_eq!(worker_rounds.len(), 2);
    assert_eq!(worker_rounds[0].0, worker_rounds[1].0);
    assert!(
        worker_rounds[1]
            .1
            .contains("The verifier reports the condition is not yet met")
            && worker_rounds[1].1.contains("scripted")
    );

    let verdicts: Vec<_> = prompts
        .iter()
        .filter(|(_, t)| t.contains("independent verifier"))
        .collect();
    assert_eq!(verdicts.len(), 2);
    assert!(verdicts.iter().all(|(s, _)| *s != worker_rounds[0].0));
    assert_ne!(verdicts[0].0, verdicts[1].0);
    assert_eq!(
        *p.live_counts.lock().unwrap(),
        vec![(false, 1), (true, 2), (false, 1), (true, 2)]
    );
    assert_eq!(env.live_count().await, 0);
    assert_eq!(
        env.eng.replay(env.lead).await.unwrap().iter()
            .filter(|e| matches!(e.event, Event::AgentRegistered { agent_session, .. } if agent_session == worker_rounds[0].0))
            .count(),
        1
    );
}

#[tokio::test]
async fn workflow_cancellation_skips_unstarted_stages_and_stops_inflight_verifier() {
    let pre_p = RecordingProvider::new();
    let pre_env = WfEnv::new(pre_p.clone()).await;
    let pre_cancel = CancellationToken::new();
    pre_cancel.cancel();
    let pre_r = pre_env
        .run_with(
            &compile_wf("cancel.hya.md", TWO_STAGE_WORKFLOW),
            BTreeMap::from([("target".into(), "cancel".into())]),
            pre_cancel,
        )
        .await
        .unwrap();
    assert_eq!(pre_r.status, WorkflowStatus::Cancelled);
    assert!(
        pre_r
            .stages
            .iter()
            .all(|s| s.status == StageStatus::Skipped && s.session.is_none())
    );
    assert!(pre_p.prompts().is_empty());

    let loop_def = compile_wf(
        "cancellable-loop.hya.md",
        "---\nkind: Workflow\nname: cancellable-loop\ndescription: Cancel verifier.\nnodes:\n  build:\n    agent: builder\n    directive: BUILD before verifier\n    mode: loop\n    verify:\n      agent: reviewer\n      until: Tests pass.\n      max_iterations: 3\n---\nflowchart TD\n  build\n",
    );
    let p = RecordingProvider::new();
    p.hang_verifier.store(true, Ordering::SeqCst);
    let env = WfEnv::new(p.clone()).await;
    let cancel = CancellationToken::new();
    let rc = cancel.clone();
    let run = tokio::spawn(async move { env.run_with(&loop_def, BTreeMap::new(), rc).await });
    p.wait_started().await;
    cancel.cancel();
    let report = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .expect("cancels promptly")
        .unwrap()
        .unwrap();
    assert_eq!(
        (report.status, report.stages[0].status.to_string()),
        (WorkflowStatus::Cancelled, "cancelled".into())
    );
    assert!(report.stages[0].output.contains("WORKER_DONE"));
}

// ---------------------------------------------------------------------------
// 5. Resident actor identity, session reuse, failure evidence, and cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resident_actor_identity_session_reuse_and_route_matching() {
    let src = "---\nkind: Workflow\nname: resident-flow\ndescription: Reuse resident.\nnodes:\n  draft:\n    agent: resident-planner\n    actor: planner\n    directive: Draft the plan.\n    model:\n      id: fake/resident\n  refine:\n    agent: resident-planner\n    actor: planner\n    directive: Refine the plan.\n    model:\n      id: fake/resident\n---\nflowchart TD\n  draft --> refine\n";
    let wf = compile_wf("resident.hya.md", src);
    let mismatched = compile_wf(
        "mismatch.hya.md",
        &src.replacen(
            "Refine the plan.\n    model:\n      id: fake/resident",
            "Refine the plan.\n    model:\n      id: fake/other",
            1,
        ),
    );
    let p = RecordingProvider::new();
    let cat = support::test_catalog(&[("resident-planner", AgentRole::Subagent, &[])]);
    let env = WfEnv::custom(
        p.clone(),
        cat,
        SubagentLimits::default(),
        None,
        None,
        Path::new("/tmp"),
    )
    .await;
    let routing = hya_engine::WorkflowRoutingContext::new(
        Arc::new(hya_engine::CategoryRegistry::default()),
        env.eng.provider_router(),
    );
    let rctx = || WorkflowRunContext {
        routing: Some(routing.clone()),
        ..env.ctx(BTreeMap::new())
    };

    let err = run_workflow(
        env.eng.clone(),
        env.lead,
        &mismatched,
        rctx(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("identical effective worker route"),
        "{err}"
    );
    assert!(p.prompts().is_empty());

    let report = tokio::time::timeout(
        Duration::from_secs(2),
        run_workflow(
            env.eng.clone(),
            env.lead,
            &wf,
            rctx(),
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(report.status, WorkflowStatus::Completed);
    assert_eq!(report.stages[0].session, report.stages[1].session);
    let sid = report.stages[0].session.as_deref().unwrap();
    assert_eq!(
        p.prompts()
            .iter()
            .filter(|(c, _)| c.to_string() == sid)
            .count(),
        2
    );
    let sys = p.systems.lock().unwrap().clone();
    assert_eq!(sys.len(), 2);
    assert!(
        sys[0]
            .1
            .as_deref()
            .is_some_and(|s| s.contains("resident-planner prompt") && s.contains("stage: draft"))
    );
    assert!(
        sys[1]
            .1
            .as_deref()
            .is_some_and(|s| s.contains("resident-planner prompt") && s.contains("stage: refine"))
    );
    let root = env.eng.read_projection(env.lead).await.unwrap();
    assert_eq!(root.team.inboxes.values().map(Vec::len).sum::<usize>(), 4);
    let res = root
        .team
        .roster
        .values()
        .find(|e| e.session.to_string() == sid)
        .unwrap();
    assert!(res.resident_cursor >= 2 && res.resident_work.is_none());
    assert_eq!(
        env.eng
            .replay(env.lead)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(e.event, Event::MailSent { .. }))
            .count(),
        2
    );

    // Actor keys — not agent ids — control Session reuse.
    let id_wf = compile_wf(
        "identity.hya.md",
        "---\nkind: Workflow\nname: identity-flow\ndescription: Actor identity.\nnodes:\n  first:\n    agent: builder\n    directive: First.\n  second:\n    agent: builder\n    directive: Second.\n  actor_a:\n    agent: resident-planner\n    actor: alpha\n    directive: Alpha.\n  actor_b:\n    agent: resident-planner\n    actor: beta\n    directive: Beta.\n---\nflowchart TD\n  first --> second\n  second --> actor_a & actor_b\n",
    );
    let id_cat = support::test_catalog(&[
        ("builder", AgentRole::Subagent, &[]),
        ("resident-planner", AgentRole::Subagent, &[]),
    ]);
    let id_env = WfEnv::custom(
        RecordingProvider::new(),
        id_cat,
        SubagentLimits::default(),
        None,
        None,
        Path::new("/tmp"),
    )
    .await;
    let id_r = id_env.run(&id_wf).await.unwrap();
    assert_eq!(id_r.status, WorkflowStatus::Completed);
    assert_ne!(id_r.stages[0].session, id_r.stages[1].session);
    assert_ne!(id_r.stages[2].session, id_r.stages[3].session);
    assert_eq!(
        id_env
            .eng
            .read_projection(id_env.lead)
            .await
            .unwrap()
            .team
            .roster
            .values()
            .filter(|e| e.agent_type.as_str() == "resident-planner")
            .count(),
        2
    );
}

#[tokio::test]
async fn resident_stage_failure_and_cancellation_settle_cleanly_to_archive() {
    let cat = support::test_catalog(&[("resident-worker", AgentRole::Subagent, &[])]);
    let fail_wf = compile_wf(
        "failed-resident.hya.md",
        "---\nkind: Workflow\nname: failed-resident\ndescription: Resident failure.\nnodes:\n  fail:\n    agent: resident-worker\n    actor: worker\n    directive: FAIL_ME resident turn.\n---\nflowchart TD\n  fail\n",
    );
    let fenv = WfEnv::custom(
        RecordingProvider::new(),
        cat.clone(),
        SubagentLimits::default(),
        None,
        None,
        Path::new("/tmp"),
    )
    .await;
    let fr = fenv.run(&fail_wf).await.unwrap();
    assert_eq!(
        (fr.status, fr.stages[0].status),
        (WorkflowStatus::Failed, StageStatus::Failed)
    );
    fenv.assert_archived_resident("resident-worker").await;

    let cancel_wf = compile_wf(
        "cancel-resident.hya.md",
        "---\nkind: Workflow\nname: cancel-resident\ndescription: Cancel resident.\nnodes:\n  work:\n    agent: resident-worker\n    actor: worker\n    directive: Hang until cancelled.\n---\nflowchart TD\n  work\n",
    );
    let hanging = RecordingProvider::new();
    hanging.hang_all.store(true, Ordering::SeqCst);
    let cenv = WfEnv::custom(
        hanging.clone(),
        cat,
        SubagentLimits::default(),
        None,
        None,
        Path::new("/tmp"),
    )
    .await;
    let cancel = CancellationToken::new();
    let (eng_t, lead_t, ctx_t, rc) = (
        cenv.eng.clone(),
        cenv.lead,
        cenv.ctx(BTreeMap::new()),
        cancel.clone(),
    );
    let run = tokio::spawn(async move { run_workflow(eng_t, lead_t, &cancel_wf, ctx_t, rc).await });
    hanging.wait_started().await;
    cancel.cancel();
    let cr = tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        (cr.status, cr.stages[0].status),
        (WorkflowStatus::Cancelled, StageStatus::Cancelled)
    );
    cenv.assert_archived_resident("resident-worker").await;
}

// ---------------------------------------------------------------------------
// 6. Per-member execution context resolution: can_spawn roster & sidecars
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stage_members_resolve_their_own_can_spawn_roster() {
    let isolated = compile_wf(
        "isolated.hya.md",
        "---\nkind: Workflow\nname: isolated-flow\ndescription: No delegation.\nnodes:\n  only:\n    agent: worker\n    directive: ROSTER_PROBE_ISOLATED\n---\nflowchart TD\n  only\n",
    );
    let delegating = compile_wf(
        "delegating.hya.md",
        "---\nkind: Workflow\nname: delegating-flow\ndescription: Delegate to helper.\nnodes:\n  spawn_step:\n    agent: delegate\n    directive: ROSTER_PROBE_DELEGATE\n---\nflowchart TD\n  spawn_step\n",
    );
    let cat = support::test_catalog(&[
        ("helper", AgentRole::Subagent, &[]),
        ("worker", AgentRole::Subagent, &[]),
        ("delegate", AgentRole::Subagent, &["helper"]),
    ]);
    let workdir = support::TestDir::new("workflow-roster");
    let mut env = WfEnv::custom(
        RecordingProvider::with_probe("list_agents"),
        cat,
        SubagentLimits::default(),
        None,
        None,
        workdir.path(),
    )
    .await;

    let caller = env
        .eng
        .agent_roster_for_binding(&env.binding, "hya-main")
        .unwrap();
    assert!(
        ["worker", "helper", "delegate"]
            .iter()
            .all(|n| caller.iter().any(|a| a.name == *n))
    );
    env.binding.resolve_spawn("hya-main", "worker").unwrap();
    env.binding.resolve_spawn("hya-main", "delegate").unwrap();

    let iso = env.run(&isolated).await.unwrap();
    assert_eq!(iso.status, WorkflowStatus::Completed);
    let iso_child: SessionId = iso.stages[0].session.as_deref().unwrap().parse().unwrap();
    let iso_res = tool_results_for(&mut env.events, iso_child);
    assert_eq!(iso_res.len(), 1);
    assert!(iso_res[0].as_ref().unwrap().contains("No agents available"));

    let del = env.run(&delegating).await.unwrap();
    assert_eq!(del.status, WorkflowStatus::Completed);
    let del_child: SessionId = del.stages[0].session.as_deref().unwrap().parse().unwrap();
    assert_ne!(del_child, iso_child);
    let del_res = tool_results_for(&mut env.events, del_child);
    assert_eq!(del_res.len(), 1);
    let listing = del_res[0].as_ref().unwrap();
    assert!(
        listing.contains("- helper") && !listing.contains("worker") && !listing.contains("- build")
    );
}

#[tokio::test]
async fn private_loop_worker_and_verifier_reuse_target_execution_contexts() {
    let def = compile_wf(
        "sidecar-loop.hya.md",
        "---\nkind: Workflow\nname: sidecar-loop-flow\ndescription: Verified Stage with sidecar.\nnodes:\n  make:\n    agent: builder\n    directive: SIDECAR_PROBE\n    mode: loop\n    verify:\n      agent: reviewer\n      until: Marker reached.\n      max_iterations: 3\n---\nflowchart TD\n  make\n",
    );
    let sidecar_env = Arc::new(OnlyBuilderSidecarEnv(Mutex::new(Vec::new())));
    let cat = sidecar_probe_catalog();
    assert!(cat.resolve_spawn("hya-main", "builder").is_err());
    assert!(cat.resolve_spawn("hya-main", "reviewer").is_err());
    let workdir = support::TestDir::new("workflow-sidecar");
    let mut env = WfEnv::custom(
        RecordingProvider::with_probe("sidecar_ping"),
        cat,
        SubagentLimits::default(),
        None,
        Some(sidecar_env.clone()),
        workdir.path(),
    )
    .await;

    let report = env.run(&def).await.unwrap();
    assert_eq!(report.status, WorkflowStatus::Completed);
    let stage = &report.stages[0];
    assert_eq!(
        (stage.stage.as_str(), stage.status.to_string()),
        ("make", "done".into())
    );
    assert!(stage.output.contains("PROBE_DONE") && stage.output.contains("[verified: scripted]"));
    assert_eq!(
        *sidecar_env.0.lock().unwrap(),
        vec!["builder".to_string(), "reviewer".to_string()]
    );
    let child: SessionId = stage.session.as_deref().unwrap().parse().unwrap();
    let pings = tool_results_for(&mut env.events, child);
    assert_eq!(pings.len(), 1);
    assert!(
        pings[0]
            .as_ref()
            .is_ok_and(|o| o.contains("PONG_FROM_SIDECAR"))
    );
}

// ---------------------------------------------------------------------------
// 7. Capacity overload cleanup, store link errors, wrap-up refusal/cancellation,
//    and sequential anonymous capacity release
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registration_overload_and_store_link_failure_release_all_claims() {
    for named in [false, true] {
        let actors = if named {
            ["    actor: first\n", "    actor: second\n"]
        } else {
            ["", ""]
        };
        let wf = compile_wf(
            "partial.hya.md",
            &format!(
                "---\nkind: Workflow\nname: partial\ndescription: Sibling overload.\nnodes:\n  first:\n    agent: builder\n{}    directive: First.\n  second:\n    agent: builder\n{}    directive: Second.\n---\nflowchart TD\n  first\n  second\n",
                actors[0], actors[1]
            ),
        );
        let gate = StageLinkGate::new(WorkflowMemberRole::Worker, 0);
        let limits = SubagentLimits {
            per_run_budget: 2,
            ..SubagentLimits::default()
        };
        let env = WfEnv::custom(
            RecordingProvider::new(),
            default_catalog(),
            limits,
            Some(Arc::new(RegistrationGate(vec![gate.clone()]))),
            None,
            Path::new("/tmp"),
        )
        .await;
        let root_turn = env.eng.try_begin_turn(env.lead).unwrap();
        let prepared = env.prepare(&wf, Some(WorkflowRunId::new())).await.unwrap();
        let run = tokio::spawn(prepared.execute(CancellationToken::new()));
        gate.wait_entered().await;
        let (task, handle) = env.spawn_parked("Concurrent task.").await;
        gate.unblock();
        let err = tokio::time::timeout(Duration::from_secs(3), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            err,
            WorkflowError::Engine(CoreError::Store(StoreError::CapacityExceeded { .. }))
        ));
        assert_eq!(
            env.eng.store().active_actor_ids().await.unwrap(),
            vec![task]
        );
        let root = env.eng.read_projection(env.lead).await.unwrap();
        assert_eq!(root.team.archived.len(), 1);
        let parked = root.team.archived.values().next().unwrap().session;
        assert!(
            !env.eng
                .replay(parked)
                .await
                .unwrap()
                .iter()
                .any(|e| matches!(e.event, Event::MessageStarted { .. }))
        );
        env.sup.end_member(env.lead, &handle).await.unwrap();
        assert_eq!(env.live_count().await, 0);
        drop(root_turn);
        env.drain().await;
    }

    let p = RecordingProvider::new();
    p.script_verdict(false);
    let env = WfEnv::new(p).await;
    let wf = compile_wf(
        "store.hya.md",
        "---\nkind: Workflow\nname: store\ndescription: Typed link failure\nnodes:\n  work:\n    agent: builder\n    directive: Work.\n    mode: loop\n    verify:\n      agent: reviewer\n      until: More work.\n      max_iterations: 2\n---\nflowchart TD\n  work\n",
    );
    let mut tx = env.eng.store().begin_transaction().await.unwrap();
    sqlx::query("CREATE TRIGGER reject_loop_link BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.type') = 'workflow_stage_member_linked' AND json_extract(NEW.payload, '$.iteration') = 1 AND json_extract(NEW.payload, '$.role') = 'worker' BEGIN SELECT RAISE(ABORT, 'review W3 link failure'); END")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let res = env
        .prepare(&wf, Some(WorkflowRunId::new()))
        .await
        .unwrap()
        .execute(CancellationToken::new())
        .await;
    assert!(
        matches!(res, Err(WorkflowError::Engine(CoreError::Store(_)))),
        "{res:?}"
    );
    assert_eq!(env.live_count().await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loop_wrap_up_capacity_refusal_and_cancellation_never_complete_stage() {
    let wrap_wf = compile_wf(
        "wrap.hya.md",
        "---\nkind: Workflow\nname: wrap\ndescription: Wrap-up revival\nnodes:\n  work:\n    agent: builder\n    directive: Work.\n    mode: loop\n    verify:\n      agent: reviewer\n      until: More work.\n      max_iterations: 1\n---\nflowchart TD\n  work\n",
    );

    let v_gate = StageLinkGate::new(WorkflowMemberRole::Verifier, 0);
    let w_gate = StageLinkGate::new(WorkflowMemberRole::Worker, 1);
    let p = RecordingProvider::new();
    p.script_verdict(false);
    let limits = SubagentLimits {
        per_run_budget: 2,
        ..SubagentLimits::default()
    };
    let env = WfEnv::custom(
        p,
        default_catalog(),
        limits,
        Some(Arc::new(RegistrationGate(vec![
            v_gate.clone(),
            w_gate.clone(),
        ]))),
        None,
        Path::new("/tmp"),
    )
    .await;
    let root_turn = env.eng.try_begin_turn(env.lead).unwrap();
    let run = tokio::spawn(
        env.prepare(&wrap_wf, Some(WorkflowRunId::new()))
            .await
            .unwrap()
            .execute(CancellationToken::new()),
    );
    v_gate.wait_entered().await;
    let worker_handle = env
        .eng
        .read_projection(env.lead)
        .await
        .unwrap()
        .team
        .roster
        .values()
        .find(|e| e.agent_type.as_str() == "builder")
        .unwrap()
        .handle
        .clone();
    env.sup.end_member(env.lead, &worker_handle).await.unwrap();
    v_gate.unblock();
    w_gate.wait_entered().await;
    let competitors = [
        env.spawn_parked("competing").await.1,
        env.spawn_parked("competing").await.1,
    ];
    w_gate.unblock();
    let res = tokio::time::timeout(Duration::from_secs(3), run)
        .await
        .unwrap()
        .unwrap();
    let completed = env.has_completed_stage_event().await;
    for h in competitors {
        env.sup.end_member(env.lead, &h).await.unwrap();
    }
    drop(root_turn);
    env.drain().await;
    assert!(matches!(
        res,
        Err(WorkflowError::Engine(CoreError::Store(
            StoreError::CapacityExceeded { .. }
        )))
    ));
    assert!(!completed);

    let c_gate = StageLinkGate::new(WorkflowMemberRole::Worker, 1);
    let cp = RecordingProvider::new();
    cp.script_verdict(false);
    let cenv = WfEnv::custom(
        cp,
        default_catalog(),
        SubagentLimits::default(),
        Some(Arc::new(RegistrationGate(vec![c_gate.clone()]))),
        None,
        Path::new("/tmp"),
    )
    .await;
    let cancel = CancellationToken::new();
    let crun = tokio::spawn(
        cenv.prepare(&wrap_wf, Some(WorkflowRunId::new()))
            .await
            .unwrap()
            .execute(cancel.clone()),
    );
    c_gate.wait_entered().await;
    cancel.cancel();
    c_gate.unblock();
    let report = tokio::time::timeout(Duration::from_secs(3), crun)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        (report.status, report.stages[0].status.to_string()),
        (WorkflowStatus::Cancelled, "cancelled".into())
    );
    assert!(report.stages[0].output.contains("WORKER_DONE"));
    assert!(!cenv.has_completed_stage_event().await);
    assert_eq!(cenv.live_count().await, 0);
}

#[tokio::test]
async fn sequential_runs_and_stages_release_anonymous_capacity_before_successor() {
    let renv = WfEnv::with_budget(RecordingProvider::new(), 2).await;
    let rwf = compile_wf("repeat.hya.md", TWO_STAGE_WORKFLOW);
    for _ in 0..2 {
        let report = renv
            .run_with(
                &rwf,
                BTreeMap::from([("target".into(), "repeat".into())]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(report.status, WorkflowStatus::Completed);
    }
    assert_eq!(renv.live_count().await, 0);

    for loop_verdict in [None, Some(true), Some(false)] {
        let p = RecordingProvider::new();
        if let Some(met) = loop_verdict {
            p.script_verdict(met);
        }
        let env = WfEnv::new(p.clone()).await;
        *p.capacity.lock().unwrap() = Some((env.eng.store().clone(), env.lead));
        let verify = if loop_verdict.is_some() {
            "    mode: loop\n    verify:\n      agent: reviewer\n      until: Done.\n      max_iterations: 1\n"
        } else {
            ""
        };
        let wf = compile_wf(
            "stage-end.hya.md",
            &format!(
                "---\nkind: Workflow\nname: stage-end\ndescription: Stage lifetime\nnodes:\n  first:\n    agent: builder\n    directive: First.\n{verify}  next:\n    agent: builder\n    directive: Next.\n---\nflowchart TD\n  first --> next\n"
            ),
        );
        assert_eq!(
            env.run(&wf).await.unwrap().status,
            WorkflowStatus::Completed
        );
        assert_eq!(p.live_counts.lock().unwrap().last(), Some(&(false, 1)));
        assert_eq!(env.live_count().await, 0);
    }
}

// ---------------------------------------------------------------------------
// 8. Isolated deadline-notice contract (process-global deadline install)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn workflow_worker_and_verifier_deadline_notices_follow_offered_report_tool() {
    support::run_isolated_subproc(
        "deadline_notice",
        "workflow::workflow_worker_and_verifier_deadline_notices_follow_offered_report_tool",
        || async {
            hya_tool::deadline::install(Duration::from_secs(30));
            let workflow = compile_wf("deadline.hya.md", "---\nkind: Workflow\nname: deadline\ndescription: Offered report capability\nnodes:\n  loop:\n    agent: hya-task\n    directive: Work.\n    mode: loop\n    verify:\n      agent: hya-reviewer\n      until: Finished.\n      max_iterations: 2\n---\nflowchart TD\n  loop\n");
            let denied_catalog = {
                let src = hya_bundle::BundleSource::new("denied-report", vec![hya_bundle::SourceFile::new("bundle.yaml", "kind: Bundle\nidentity: { id: acme/denied-report, version: 1.0.0, publisher: acme }\nchannels:\n  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [send, steer, follow_up, resident_mail], scope: vertical, retention: team_session }\n")]);
                let bc = hya_bundle::BundleCatalog::from_prepared(hya_bundle::prepare_package(src).unwrap().bundles()).unwrap();
                Arc::new(AgentCatalog::new(Arc::new(bc)).unwrap())
            };
            let tools = builtin_tools();
            let allowed_runtime = support::test_runtime(tools.clone());
            let denied_runtime = support::test_runtime_with_catalog(tools, denied_catalog);
            let dir = support::TestDir::new("workflow-deadline");
            let mut wrong_notices = Vec::new();
            for (think_first, governed, connected, allowed) in [false, true].into_iter().flat_map(|t| [(t, false, true, true), (t, true, true, true), (t, true, false, true), (t, true, true, false)]) {
                let tool = vec![FakeStep::ToolCall { name: "glob".into(), input: json!({"pattern": "*"}) }, FakeStep::Finish(FinishReason::ToolCalls)];
                let ans = vec![FakeStep::Text(r#"{"met":true,"reason":"done"}"#.into()), FakeStep::Finish(FinishReason::Stop)];
                let scripts = if think_first { vec![ans.clone(), ans] } else { vec![tool.clone(), ans.clone(), tool, ans] };
                let provider = Arc::new(NoticeProvider { inner: FakeProvider::scripted_turns(scripts), think_first, report_offered: Mutex::default(), last_user: Mutex::default() });
                let (perm, _) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(Action::Tool, "glob", Mode::Allow), Rule::new(Action::Glob, "*", Mode::Allow)]));
                let runtime = if allowed { allowed_runtime.clone() } else { denied_runtime.clone() };
                let (lifecycle, lifecycle_rx) = hya_tool::LifecyclePlane::new();
                let mut eng = SessionEngine::new(SessionStore::connect_memory().await.unwrap(), Arc::new(ProviderRouter::new().with(provider.clone())), runtime, perm, EventBus::default())
                    .with_round_retry_waits(&support::ZERO_RETRY_WAITS);
                if connected {
                    eng = eng.with_lifecycle(lifecycle);
                }
                if governed {
                    eng = eng.with_governor(SubagentGovernor::new(SubagentLimits::default()));
                }
                let eng = Arc::new(eng);
                let root = support::create_root_session(&eng, "hya-main", "fake", dir.path()).await;
                let sup = ResidentSupervisor::start(eng.clone());
                let lc_stop = CancellationToken::new();
                let lc_task = tokio::spawn(hya_engine::run_lifecycle_service(eng.clone(), sup.clone(), lifecycle_rx, lc_stop.clone()));
                let ctx = WorkflowRunContext {
                    binding: eng.bind_runtime(dir.path()).unwrap(),
                    caller: "hya-main".into(),
                    base_agent: support::agent_spec("hya-main", "fake", dir.path()),
                    inputs: BTreeMap::new(),
                    resident_supervisor: Some(sup.clone()),
                    routing: None,
                };
                assert_eq!(run_workflow(eng.clone(), root, &workflow, ctx, CancellationToken::new()).await.unwrap().status, WorkflowStatus::Completed);
                let members = eng.read_projection(root).await.unwrap().session.members;
                assert_eq!(members.len(), 2);
                for member in members {
                    let child = member.child.unwrap();
                    assert_eq!(provider.report_offered.lock().unwrap()[&child], governed);
                    let notices: Vec<_> = eng.replay(child).await.unwrap().into_iter().filter_map(|e| match e.event {
                        Event::ToolResult { output, .. } if output.to_string().contains("HEADLESS RUN") => Some(output.to_string()),
                        Event::TextDelta { delta, .. } if delta.starts_with(REASONING_DEADLINE_NOTICE_PREFIX) => Some(delta),
                        _ => None,
                    }).collect();
                    assert_eq!(notices.len(), 1, "child {child}: {notices:?}");
                    if think_first {
                        assert!(notices[0].starts_with(REASONING_DEADLINE_NOTICE_PREFIX));
                        let sent: Vec<_> = provider.last_user.lock().unwrap().iter().filter(|(s, _)| *s == child).map(|(_, t)| t.clone()).collect();
                        assert_eq!(sent.len(), 2);
                        assert_eq!(sent[1], notices[0]);
                    }
                    if notices[0].contains("call `report`") != (governed && connected && allowed) {
                        wrong_notices.push(format!("think_first={think_first}, governed={governed}, connected={connected}, allowed={allowed}, child={child}: {}", notices[0]));
                    }
                }
                sup.drain(FinishCause::Shutdown, hya_engine::DRAIN_DEADLINE).await;
                lc_stop.cancel();
                lc_task.await.unwrap();
            }
            assert!(wrong_notices.is_empty(), "unusable report instructions: {wrong_notices:#?}");
        },
    )
    .await;
}
