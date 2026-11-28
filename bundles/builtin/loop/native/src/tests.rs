#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hya_bundle::{Bundle, BundleSource, PreparedAgent, SourceFile, prepare_package};
use hya_proto::{
    ActorClaim, AgentName, Envelope, Event, EventSeq, FinishReason, Message, MessageId, ModelRef,
    PartId, Projection, Role, SessionId,
};
use hya_runtime_abi::{
    BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
    BundleClaimRequest, BundleStateEntry, DriverRunOutcome, ExtensionEntry, ExtensionKind,
    HostBundleStorage, HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry,
    HostModelCaller, HostRuntime, HostSessionStore, HostTurnRunner, LoopDriver, LoopRunRequest,
    ModelCallRequest, ModelCallResponse, ModelStreamChunk, ModelWindowInfo, NativeLoopPlanInput,
    NativePlannerOutput, NativeService, NativeVerifierVerdict, SessionListFilter,
    SessionSummaryMeta, TurnOutcome, TurnSpawnRequest, TurnStartRequest, TurnStatus, abi_digest_v2,
};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::*;

// ---------------------------------------------------------------------------
// Mock HostRuntime for pure ABI v2 unit testing
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockHostState {
    projections: HashMap<SessionId, Arc<Projection>>,
    sessions: HashMap<SessionId, SessionSummaryMeta>,
    model_replies: Vec<Result<String, HostError>>,
    model_requests: Vec<ModelCallRequest>,
    spawned_prompts: Vec<String>,
    events: Vec<(SessionId, String, String, Value)>,
    bundle: Option<Bundle>,
    should_stop: Option<String>,
    should_stop_calls: usize,
    hook_verdict: Option<Result<Option<NativeVerifierVerdict>, HostError>>,
    hook_plan: Option<Result<Option<NativePlannerOutput>, HostError>>,
}

struct MockHostRuntime {
    state: Mutex<MockHostState>,
}

impl MockHostRuntime {
    fn new(state: MockHostState) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(state),
        })
    }

    fn with_model_replies(replies: Vec<Result<String, HostError>>) -> Arc<Self> {
        Self::new(MockHostState {
            model_replies: replies,
            ..MockHostState::default()
        })
    }
}

#[async_trait]
impl HostSessionStore for MockHostRuntime {
    async fn session_exists(&self, session: SessionId) -> Result<bool, HostError> {
        Ok(self.state.lock().unwrap().sessions.contains_key(&session))
    }

    async fn read_events(
        &self,
        _session: SessionId,
        _after_seq: Option<EventSeq>,
        _limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError> {
        Ok(Vec::new())
    }

    async fn append_events(
        &self,
        _session: SessionId,
        _claim: Option<ActorClaim>,
        _events: Vec<Event>,
    ) -> Result<Vec<Envelope>, HostError> {
        Ok(Vec::new())
    }

    async fn append_extension_event(
        &self,
        _session: SessionId,
        _bundle: &str,
        _kind: &str,
        _payload: Value,
        _claim: Option<ActorClaim>,
    ) -> Result<Envelope, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn read_projection(&self, session: SessionId) -> Result<Arc<Projection>, HostError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .projections
            .get(&session)
            .cloned()
            .unwrap_or_else(|| Arc::new(Projection::default())))
    }

    async fn read_extension_state(
        &self,
        _session: SessionId,
        _bundle: &str,
    ) -> Result<Option<Value>, HostError> {
        Ok(None)
    }

    async fn session_metadata(
        &self,
        session: SessionId,
    ) -> Result<Option<SessionSummaryMeta>, HostError> {
        Ok(self.state.lock().unwrap().sessions.get(&session).cloned())
    }

    async fn list_sessions(
        &self,
        _filter: SessionListFilter,
    ) -> Result<Vec<SessionSummaryMeta>, HostError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl HostBundleStorage for MockHostRuntime {
    async fn get_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _key: &str,
    ) -> Result<Option<BundleStateEntry>, HostError> {
        Ok(None)
    }

    async fn put_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _key: &str,
        _value: Value,
        _expected_version: Option<u64>,
    ) -> Result<BundleStateEntry, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn delete_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _key: &str,
    ) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn list_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _prefix: Option<&str>,
    ) -> Result<Vec<BundleStateEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn acquire_claim(
        &self,
        _request: BundleClaimRequest,
    ) -> Result<BundleClaimLease, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn renew_claim(
        &self,
        _lease: &BundleClaimLease,
        _ttl_millis: u64,
    ) -> Result<BundleClaimLease, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn release_claim(&self, _lease: &BundleClaimLease) -> Result<(), HostError> {
        Ok(())
    }

    async fn read_claim(
        &self,
        _bundle: &str,
        _resource_key: &str,
    ) -> Result<Option<BundleClaimLease>, HostError> {
        Ok(None)
    }
}

#[async_trait]
impl HostTurnRunner for MockHostRuntime {
    async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
        Ok(SessionId::new())
    }

    async fn run_turn(
        &self,
        request: TurnStartRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Ok(TurnOutcome {
            session: request.session,
            message_id: Some(MessageId::new()),
            finish_reason: Some(FinishReason::Stop),
            output_text: "ok".into(),
            error: None,
        })
    }

    async fn spawn_session_and_run(
        &self,
        request: TurnSpawnRequest,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        if cancel.is_cancelled() {
            return Err(HostError::Cancelled);
        }
        let child = SessionId::new();
        let mut st = self.state.lock().unwrap();
        st.spawned_prompts.push(request.prompt.clone());
        let mut proj = Projection::default();
        let user_msg = MessageId::new();
        let user_part = PartId::new();
        let assistant_msg = MessageId::new();
        let assistant_part = PartId::new();
        let events = [
            Event::MessageStarted {
                session: child,
                message: user_msg,
                role: Role::User,
                agent: None,
                model: None,
            },
            Event::TextStart {
                session: child,
                message: user_msg,
                part: user_part,
            },
            Event::TextDelta {
                session: child,
                message: user_msg,
                part: user_part,
                delta: request.prompt,
            },
            Event::MessageFinished {
                session: child,
                message: user_msg,
                role: Role::User,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
            Event::MessageStarted {
                session: child,
                message: assistant_msg,
                role: Role::Assistant,
                agent: Some(request.agent),
                model: request.model,
            },
            Event::TextStart {
                session: child,
                message: assistant_msg,
                part: assistant_part,
            },
            Event::TextDelta {
                session: child,
                message: assistant_msg,
                part: assistant_part,
                delta: "worker completed step".into(),
            },
            Event::MessageFinished {
                session: child,
                message: assistant_msg,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ];
        for (idx, event) in events.into_iter().enumerate() {
            proj.apply(&Envelope {
                seq: EventSeq(idx as u64 + 1),
                ts_millis: 0,
                event,
            });
        }
        st.projections.insert(child, Arc::new(proj));
        Ok(TurnOutcome {
            session: child,
            message_id: Some(MessageId::new()),
            finish_reason: Some(FinishReason::Stop),
            output_text: "worker completed step".into(),
            error: None,
        })
    }

    async fn steer_turn(&self, _session: SessionId, _message: String) -> Result<(), HostError> {
        Ok(())
    }

    async fn cancel_turn(
        &self,
        _session: SessionId,
        _reason: Option<String>,
    ) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn turn_status(&self, _session: SessionId) -> Result<TurnStatus, HostError> {
        Ok(TurnStatus::Idle)
    }
}

#[async_trait]
impl HostModelCaller for MockHostRuntime {
    async fn complete(
        &self,
        request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelCallResponse, HostError> {
        let mut st = self.state.lock().unwrap();
        let model = request
            .model
            .clone()
            .unwrap_or_else(|| ModelRef::new("fake/model"));
        st.model_requests.push(request);
        let reply = if st.model_replies.is_empty() {
            Ok(String::new())
        } else {
            st.model_replies.remove(0)
        }?;
        Ok(ModelCallResponse {
            model,
            text: reply,
            reasoning: None,
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Stop,
            usage: None,
        })
    }

    async fn stream(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn compact_window(
        &self,
        _model: &ModelRef,
        _messages: &[Message],
    ) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    async fn model_window(&self, _model: &ModelRef) -> Result<Option<ModelWindowInfo>, HostError> {
        Ok(None)
    }
}

#[async_trait]
impl HostEventBus for MockHostRuntime {
    fn publish(&self, _envelope: Envelope) -> Result<(), HostError> {
        Ok(())
    }

    fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
        let (tx, rx) = broadcast::channel(4);
        drop(tx);
        Ok(rx)
    }

    async fn publish_extension(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), HostError> {
        self.state.lock().unwrap().events.push((
            session,
            bundle.to_string(),
            kind.to_string(),
            payload,
        ));
        Ok(())
    }
}

#[async_trait]
impl HostCatalogReader for MockHostRuntime {
    async fn list_agents(
        &self,
        _session: Option<SessionId>,
    ) -> Result<Vec<hya_tool::AgentDef>, HostError> {
        Ok(Vec::new())
    }

    async fn get_agent(
        &self,
        _name: &AgentName,
        _session: Option<SessionId>,
    ) -> Result<Option<PreparedAgent>, HostError> {
        Ok(None)
    }

    async fn list_skills(
        &self,
        _session: Option<SessionId>,
    ) -> Result<Vec<hya_tool::SkillCatalogEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn list_bundles(&self) -> Result<Vec<BundleCatalogSummary>, HostError> {
        Ok(Vec::new())
    }

    async fn get_bundle(&self, id: &str) -> Result<Option<Bundle>, HostError> {
        let st = self.state.lock().unwrap();
        Ok(st
            .bundle
            .as_ref()
            .filter(|b| b.identity().id == id)
            .cloned())
    }

    async fn bundle_config(&self, _bundle_id: &str) -> Result<Value, HostError> {
        Ok(Value::Null)
    }

    async fn global_config(&self) -> Result<Value, HostError> {
        Ok(Value::Null)
    }
}

#[async_trait]
impl HostExtensionRegistry for MockHostRuntime {
    async fn list_extensions(
        &self,
        _kind: Option<ExtensionKind>,
    ) -> Result<Vec<ExtensionEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn has_bundle(&self, _bundle_id: &str) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn invoke_bundle_api(
        &self,
        _call: BundleApiInvocation,
    ) -> Result<BundleApiResult, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn invoke_tool(
        &self,
        _session: Option<SessionId>,
        _tool_name: &str,
        _input: Value,
        _cancel: CancellationToken,
    ) -> Result<Value, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn invoke_loop_should_stop(
        &self,
        _session: SessionId,
        _target: &str,
        _transcript: &str,
    ) -> Result<Option<String>, HostError> {
        let mut st = self.state.lock().unwrap();
        st.should_stop_calls += 1;
        Ok(st.should_stop.clone())
    }

    async fn invoke_loop_verify(
        &self,
        _session: SessionId,
        _target: &str,
        _transcript: &str,
    ) -> Result<Option<NativeVerifierVerdict>, HostError> {
        self.state
            .lock()
            .unwrap()
            .hook_verdict
            .clone()
            .unwrap_or(Ok(None))
    }

    async fn invoke_loop_plan(
        &self,
        _session: SessionId,
        _input: NativeLoopPlanInput,
    ) -> Result<Option<NativePlannerOutput>, HostError> {
        self.state
            .lock()
            .unwrap()
            .hook_plan
            .clone()
            .unwrap_or(Ok(None))
    }
}

impl HostRuntime for MockHostRuntime {
    fn session_store(&self) -> &dyn HostSessionStore {
        self
    }

    fn bundle_storage(&self) -> &dyn HostBundleStorage {
        self
    }

    fn turn_runner(&self) -> &dyn HostTurnRunner {
        self
    }

    fn model_caller(&self) -> &dyn HostModelCaller {
        self
    }

    fn event_bus(&self) -> &dyn HostEventBus {
        self
    }

    fn catalog_reader(&self) -> &dyn HostCatalogReader {
        self
    }

    fn extension_registry(&self) -> &dyn HostExtensionRegistry {
        self
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn verdict(score: u8, satisfied: bool, gaps: &[&str]) -> VerifierVerdict {
    VerifierVerdict {
        score,
        satisfied,
        evidence_quality: if satisfied {
            EvidenceQuality::Verified
        } else {
            EvidenceQuality::ClaimOnly
        },
        critical_gaps: gaps.iter().map(|s| (*s).into()).collect(),
        iteration_summary: format!("score {score}"),
        reason: "r".into(),
    }
}

fn predicate(command: &str, mode: PredicateMode) -> LoopPredicate {
    LoopPredicate {
        command: command.into(),
        mode,
        timeout: Duration::from_secs(5),
        workdir: std::env::temp_dir(),
    }
}

struct ScriptedLoop {
    verdicts: Vec<VerifierVerdict>,
    dirs: Vec<(String, bool)>,
    v_calls: Arc<AtomicUsize>,
    p_calls: Arc<AtomicUsize>,
}

impl ScriptedLoop {
    fn constant(satisfied: bool) -> Arc<Self> {
        Arc::new(Self {
            verdicts: vec![VerifierVerdict {
                score: 100,
                satisfied,
                evidence_quality: EvidenceQuality::Verified,
                critical_gaps: vec![],
                iteration_summary: "scripted".into(),
                reason: "scripted".into(),
            }],
            dirs: vec![("continue".into(), false)],
            v_calls: Arc::new(AtomicUsize::new(0)),
            p_calls: Arc::new(AtomicUsize::new(0)),
        })
    }
}

#[async_trait]
impl LoopVerifier for ScriptedLoop {
    async fn grade(&self, _: &str, _: &str) -> Result<VerifierVerdict, HostError> {
        let i = self.v_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .verdicts
            .get(i)
            .or_else(|| self.verdicts.last())
            .cloned()
            .unwrap())
    }
}

#[async_trait]
impl LoopPlanner for ScriptedLoop {
    async fn plan_next(
        &self,
        _: &str,
        _: &[String],
        _: &VerifierVerdict,
        _: &str,
    ) -> Result<PlannerOutput, HostError> {
        let i = self.p_calls.fetch_add(1, Ordering::Relaxed);
        let (directive, strategy_change) = self
            .dirs
            .get(i)
            .or_else(|| self.dirs.last())
            .cloned()
            .unwrap();
        Ok(PlannerOutput {
            directive,
            continuity_brief: "brief".into(),
            planner_notes: "notes".into(),
            strategy_change,
            change_note: String::new(),
        })
    }
}

struct CountingExecutor(Arc<AtomicUsize>);

#[async_trait]
impl IterationExecutor for CountingExecutor {
    async fn run_iteration(&self, _: &str, _: &CancellationToken) -> Result<String, HostError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(String::new())
    }
}

struct StaticStopHook {
    reason: Option<String>,
    calls: AtomicUsize,
}

#[async_trait]
impl LoopShouldStopHook for StaticStopHook {
    async fn should_stop(&self, _: &str, _: &str) -> Option<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.reason.clone()
    }
}

async fn drive_case(
    verdicts: Vec<VerifierVerdict>,
    dirs: Vec<(&str, bool)>,
    config: LoopConfig,
) -> (DriverRunOutcome, usize, usize, usize) {
    let ec = Arc::new(AtomicUsize::new(0));
    let sl = Arc::new(ScriptedLoop {
        verdicts,
        dirs: dirs.into_iter().map(|(d, s)| (d.into(), s)).collect(),
        v_calls: Arc::new(AtomicUsize::new(0)),
        p_calls: Arc::new(AtomicUsize::new(0)),
    });
    let out = drive_loop(
        &CountingExecutor(ec.clone()),
        sl.clone(),
        sl.clone(),
        "target".into(),
        config,
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    (
        out,
        sl.v_calls.load(Ordering::Relaxed),
        sl.p_calls.load(Ordering::Relaxed),
        ec.load(Ordering::Relaxed),
    )
}

fn gate_with_hook(
    verifier: Arc<ScriptedLoop>,
    hook: Option<Arc<dyn LoopShouldStopHook>>,
    pred: Option<LoopPredicate>,
) -> LoopGate {
    LoopGate::new(
        "target".into(),
        verifier,
        ScriptedLoop::constant(false),
        LoopConfig {
            predicate: pred,
            ..LoopConfig::default()
        },
    )
    .with_should_stop_hook(hook)
}

// ---------------------------------------------------------------------------
// 1. Model verifier/planner parsing & graceful degradation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn model_loop_verifier_and_planner_parsing_and_degradation_matrix() {
    let model = Some(ModelRef::new("fake/verifier"));
    struct VerifierCase {
        reply: Result<String, HostError>,
        satisfied: bool,
        score: u8,
        quality: EvidenceQuality,
        gaps: &'static [&'static str],
        summary: &'static str,
        reason_needle: &'static str,
    }
    let cases = [
        VerifierCase {
            reply: Ok(r#"{"score": 87, "satisfied": true, "evidence_quality": "verified", "critical_gaps": [], "iteration_summary": "tests green", "reason": "all criteria met"}"#.into()),
            satisfied: true,
            score: 87,
            quality: EvidenceQuality::Verified,
            gaps: &[],
            summary: "tests green",
            reason_needle: "all criteria met",
        },
        VerifierCase {
            reply: Ok("```json\n{\"score\": 40, \"satisfied\": false, \"evidence_quality\": \"claim_only\", \"critical_gaps\": [\"no tests\"], \"reason\": \"unfinished\"}\n```".into()),
            satisfied: false,
            score: 40,
            quality: EvidenceQuality::ClaimOnly,
            gaps: &["no tests"],
            summary: "",
            reason_needle: "unfinished",
        },
        VerifierCase {
            reply: Ok("I think it looks done!".into()),
            satisfied: false,
            score: 0,
            quality: EvidenceQuality::Missing,
            gaps: &[],
            summary: "",
            reason_needle: "verifier returned malformed output",
        },
        VerifierCase {
            reply: Err(HostError::Unavailable("boom".into())),
            satisfied: false,
            score: 0,
            quality: EvidenceQuality::Missing,
            gaps: &[],
            summary: "",
            reason_needle: "verifier provider failed",
        },
    ];
    for c in cases {
        let rt: Arc<dyn HostRuntime> = MockHostRuntime::with_model_replies(vec![c.reply]);
        let got = ModelLoopVerifier::new(rt, model.clone())
            .grade("target", "[User] do it\n[Assistant] work")
            .await
            .unwrap();
        assert_eq!(
            (got.satisfied, got.score, got.evidence_quality),
            (c.satisfied, c.score, c.quality)
        );
        assert_eq!(
            got.critical_gaps,
            c.gaps.iter().map(|s| (*s).to_string()).collect::<Vec<_>>()
        );
        if !c.summary.is_empty() {
            assert_eq!(got.iteration_summary, c.summary);
        }
        assert!(
            got.reason.contains(c.reason_needle),
            "reason: {}",
            got.reason
        );
    }

    let v = VerifierVerdict {
        score: 10,
        satisfied: false,
        evidence_quality: EvidenceQuality::Missing,
        critical_gaps: vec!["everything".into()],
        iteration_summary: "started".into(),
        reason: "not yet".into(),
    };
    let good_rt: Arc<dyn HostRuntime> = MockHostRuntime::with_model_replies(vec![Ok(
        r#"{"directive": "write the failing test", "continuity_brief": "branch is clean", "planner_notes": "focus on parser", "strategy_change": true}"#.into(),
    )]);
    let plan = ModelLoopPlanner::new(good_rt, model.clone())
        .plan_next("target", &[], &v, "")
        .await
        .unwrap();
    assert_eq!(
        (
            plan.directive.as_str(),
            plan.continuity_brief.as_str(),
            plan.planner_notes.as_str(),
            plan.strategy_change
        ),
        (
            "write the failing test",
            "branch is clean",
            "focus on parser",
            true
        )
    );

    let bad_rt: Arc<dyn HostRuntime> =
        MockHostRuntime::with_model_replies(vec![Ok("just keep going".into())]);
    let fb = ModelLoopPlanner::new(bad_rt, model)
        .plan_next("target", &[], &v, "prior notes")
        .await
        .unwrap();
    assert!(!fb.directive.is_empty() && !fb.strategy_change && fb.planner_notes.is_empty());
}

// ---------------------------------------------------------------------------
// 2. Hook priority and fallback via HostExtensionRegistry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hook_evaluators_take_priority_and_failures_use_fallback() {
    let session = SessionId::new();
    let hook_rt: Arc<dyn HostRuntime> = MockHostRuntime::new(MockHostState {
        hook_verdict: Some(Ok(Some(NativeVerifierVerdict {
            score: 91,
            satisfied: true,
            evidence_quality: "verified".into(),
            critical_gaps: vec![],
            iteration_summary: "hook".into(),
            reason: "hook verdict".into(),
        }))),
        hook_plan: Some(Ok(Some(NativePlannerOutput {
            directive: "hook directive".into(),
            continuity_brief: "hook".into(),
            planner_notes: String::new(),
            strategy_change: false,
            change_note: String::new(),
        }))),
        ..MockHostState::default()
    });
    let unused = ScriptedLoop::constant(false);
    let v = HookLoopVerifier::new(Arc::clone(&hook_rt), session, unused.clone())
        .grade("target", "transcript")
        .await
        .unwrap();
    assert_eq!(
        (v.reason.as_str(), unused.v_calls.load(Ordering::SeqCst)),
        ("hook verdict", 0)
    );

    let plan = HookLoopPlanner::new(hook_rt, session, unused)
        .plan_next("target", &[], &v, "")
        .await
        .unwrap();
    assert_eq!(plan.directive, "hook directive");

    let err_rt: Arc<dyn HostRuntime> = MockHostRuntime::new(MockHostState {
        hook_verdict: Some(Err(HostError::Internal("hook crashed".into()))),
        ..MockHostState::default()
    });
    let fb = ScriptedLoop::constant(true);
    assert!(
        HookLoopVerifier::new(err_rt, session, fb.clone())
            .grade("target", "transcript")
            .await
            .unwrap()
            .satisfied
    );
    assert_eq!(fb.v_calls.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// 3. LoopGate: should_stop, predicate precedence, iteration, stalls, cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn loop_gate_should_stop_hook_and_broken_predicate_precedence() {
    let v1 = ScriptedLoop::constant(true);
    let h1 = Arc::new(StaticStopHook {
        reason: Some("worker says done".into()),
        calls: AtomicUsize::new(0),
    });
    match gate_with_hook(v1.clone(), Some(h1.clone()), None)
        .judge("[User] do it\n[Assistant] work", &CancellationToken::new())
        .await
        .unwrap()
    {
        GateOutcome::Stop { reason } => assert_eq!(reason, "loop.should_stop: worker says done"),
        _ => panic!("expected hook stop"),
    }
    assert_eq!(
        (
            h1.calls.load(Ordering::SeqCst),
            v1.v_calls.load(Ordering::SeqCst)
        ),
        (1, 0)
    );

    for hook in [
        Some(Arc::new(StaticStopHook {
            reason: None,
            calls: AtomicUsize::new(0),
        }) as Arc<dyn LoopShouldStopHook>),
        None,
    ] {
        let v = ScriptedLoop::constant(true);
        match gate_with_hook(v.clone(), hook, None)
            .judge("[User] do it\n[Assistant] work", &CancellationToken::new())
            .await
            .unwrap()
        {
            GateOutcome::Stop { reason } => assert_eq!(reason, "satisfied: score 100"),
            _ => panic!("expected satisfied stop"),
        }
        assert_eq!(v.v_calls.load(Ordering::SeqCst), 1);
    }

    let hook = Arc::new(StaticStopHook {
        reason: Some("worker says done".into()),
        calls: AtomicUsize::new(0),
    });
    let broken = LoopPredicate::new("exit 127", PredicateMode::Until, std::env::temp_dir());
    match gate_with_hook(ScriptedLoop::constant(false), Some(hook), Some(broken))
        .judge("[User] do it\n[Assistant] work", &CancellationToken::new())
        .await
        .unwrap()
    {
        GateOutcome::InvalidCondition { reason } => {
            assert!(reason.starts_with("broken condition"), "{reason}")
        }
        _ => panic!("expected broken-condition invalid outcome"),
    }
}

#[tokio::test]
async fn loop_gate_iterates_until_satisfied_and_skips_planner_on_terminal_success() {
    let (out, vc, pc, ec) = drive_case(
        vec![
            verdict(40, false, &["a"]),
            verdict(76, false, &["b"]),
            verdict(93, true, &[]),
        ],
        vec![("d1", false), ("d2", false), ("d3", false)],
        LoopConfig::default(),
    )
    .await;
    assert!(matches!(
        out,
        DriverRunOutcome::Achieved { iterations: 3, .. }
    ));
    assert_eq!((vc, pc, ec), (3, 2, 3));

    let (imm, _, pc2, _) = drive_case(
        vec![verdict(95, true, &[])],
        vec![("d", false)],
        LoopConfig::default(),
    )
    .await;
    assert!(matches!(
        imm,
        DriverRunOutcome::Achieved { iterations: 1, .. }
    ));
    assert_eq!(pc2, 0);
}

#[tokio::test]
async fn loop_gate_budget_exhaustion_exact_budget_and_validation_bounds() {
    let dirs = vec![("d1", false), ("d2", false), ("d3", false)];
    let (exhausted, _, _, _) = drive_case(
        vec![verdict(50, false, &[])],
        dirs.clone(),
        LoopConfig {
            budget: 3,
            max_no_progress: 0,
            ..LoopConfig::default()
        },
    )
    .await;
    assert_eq!(
        exhausted,
        DriverRunOutcome::BudgetLimited {
            iterations: 3,
            which: "max_iterations".into(),
        }
    );

    let (exact, _, _, ec) = drive_case(
        vec![verdict(95, true, &[])],
        dirs,
        LoopConfig {
            budget: 3,
            stop_when_satisfied: false,
            max_no_progress: 0,
            ..LoopConfig::default()
        },
    )
    .await;
    assert_eq!(
        exact,
        DriverRunOutcome::BudgetLimited {
            iterations: 3,
            which: "max_iterations".into(),
        }
    );
    assert_eq!(ec, 4);

    assert!(
        validate_budget(&LoopConfig {
            budget: 101,
            ..LoopConfig::default()
        })
        .is_err()
    );
    assert!(
        validate_budget(&LoopConfig {
            budget: 0,
            ..LoopConfig::default()
        })
        .is_err()
    );
    assert!(
        validate_budget(&LoopConfig {
            budget: 10,
            ..LoopConfig::default()
        })
        .is_ok()
    );
    assert_eq!((clamp_budget(0), clamp_budget(7)), (1, 7));
    assert_eq!(clamp_budget(500), HARD_MAX_ITERATIONS);
}

#[tokio::test]
async fn loop_gate_stall_guards_detect_no_progress_and_repeated_directive() {
    let (np, _, _, _) = drive_case(
        vec![verdict(50, false, &["x"])],
        vec![("d1", false), ("d2", false)],
        LoopConfig::default(),
    )
    .await;
    assert_eq!(np, DriverRunOutcome::Stalled { iterations: 3 });

    let (rep, _, _, _) = drive_case(
        vec![verdict(50, false, &["g"])],
        vec![("same", false)],
        LoopConfig {
            budget: 10,
            max_no_progress: 0,
            ..LoopConfig::default()
        },
    )
    .await;
    assert_eq!(rep, DriverRunOutcome::Stalled { iterations: 2 });
}

#[tokio::test]
async fn loop_gate_cancellation_interrupts_predicate_wait() {
    let cancel = CancellationToken::new();
    let sl = Arc::new(ScriptedLoop {
        verdicts: vec![verdict(0, false, &[])],
        dirs: vec![("next".into(), false)],
        v_calls: Arc::new(AtomicUsize::new(0)),
        p_calls: Arc::new(AtomicUsize::new(0)),
    });
    let cfg = LoopConfig {
        predicate: Some(LoopPredicate::new(
            "exec sleep 2",
            PredicateMode::Until,
            std::env::temp_dir(),
        )),
        ..LoopConfig::default()
    };
    let exec = CountingExecutor(Arc::new(AtomicUsize::new(0)));
    let run = drive_loop(
        &exec,
        sl.clone(),
        sl,
        "target".into(),
        cfg,
        cancel.clone(),
        None,
    );
    let trigger = async {
        tokio::task::yield_now().await;
        cancel.cancel();
    };
    let (outcome, ()) = tokio::join!(run, trigger);
    assert_eq!(outcome.unwrap(), DriverRunOutcome::Cancelled);
}

// ---------------------------------------------------------------------------
// 4. Deterministic predicates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn loop_predicate_gate_table_until_while_broken_and_timeout() {
    enum Expect {
        Stop(&'static str),
        Continue,
        Invalid(&'static str),
    }

    for (cmd, mode, verifier_sat, expected) in [
        ("true", PredicateMode::Until, true, Expect::Stop("until")),
        ("false", PredicateMode::Until, true, Expect::Continue),
        ("false", PredicateMode::While, true, Expect::Stop("while")),
        (
            "exit 127",
            PredicateMode::Until,
            false,
            Expect::Invalid("broken condition"),
        ),
    ] {
        let gate = gate_with_hook(
            ScriptedLoop::constant(verifier_sat),
            None,
            Some(predicate(cmd, mode)),
        );
        let out = gate
            .judge("transcript", &CancellationToken::new())
            .await
            .unwrap();
        match (expected, out) {
            (Expect::Stop(n), GateOutcome::Stop { reason }) => {
                assert!(reason.contains(n), "{cmd}: {reason}")
            }
            (Expect::Continue, GateOutcome::Continue { .. }) => {}
            (Expect::Invalid(n), GateOutcome::InvalidCondition { reason }) => {
                assert!(reason.contains(n), "{cmd}: {reason}")
            }
            _ => panic!("unexpected gate outcome for `{cmd}`"),
        }
    }

    let mut timed = predicate("exec sleep 1", PredicateMode::Until);
    timed.timeout = Duration::from_millis(50);
    match gate_with_hook(ScriptedLoop::constant(false), None, Some(timed))
        .judge("transcript", &CancellationToken::new())
        .await
        .unwrap()
    {
        GateOutcome::InvalidCondition { reason } => {
            assert!(reason.contains("broken condition"), "{reason}")
        }
        _ => panic!("expected invalid condition on timeout"),
    }
}

#[tokio::test]
async fn loop_predicate_unit_evaluation_outcomes() {
    assert!(matches!(
        predicate("true", PredicateMode::Until).evaluate().await,
        LoopPredicateOutcome::Satisfied { .. }
    ));
    assert!(matches!(
        predicate("false", PredicateMode::Until).evaluate().await,
        LoopPredicateOutcome::Continue
    ));
    match predicate("exit 42", PredicateMode::Until).evaluate().await {
        LoopPredicateOutcome::Broken { detail } => assert!(detail.contains("42"), "{detail}"),
        other => panic!("expected broken, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 5. NativeLoopDriver + LoopService + ABI v2 registration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn native_loop_driver_runs_with_host_runtime_skills_events_and_service_health() {
    let prepared = prepare_package(BundleSource::new(
        "loop",
        vec![
            SourceFile::new(
                "bundle.yaml",
                "kind: Bundle\nschema_version: 2\nidentity: { id: hya/loop, version: 1.0.0, publisher: hya }\nresources:\n  skills:\n    - { id: loop-verifier-prompt, path: skills/v/SKILL.md }\n    - { id: loop-planner-prompt, path: skills/p/SKILL.md }\n",
            ),
            SourceFile::new(
                "skills/v/SKILL.md",
                "---\nname: loop-verifier-prompt\ndescription: v\n---\nCUSTOM VERIFIER PROMPT",
            ),
            SourceFile::new(
                "skills/p/SKILL.md",
                "---\nname: loop-planner-prompt\ndescription: p\n---\nCUSTOM PLANNER PROMPT",
            ),
        ],
    ))
    .unwrap();
    let session = SessionId::new();
    let mock = MockHostRuntime::new(MockHostState {
        bundle: Some(prepared.bundles()[0].clone()),
        model_replies: vec![
            Ok(r#"{"score": 45, "satisfied": false, "evidence_quality": "claim_only", "critical_gaps": ["missing test"], "iteration_summary": "step 1", "reason": "need test"}"#.into()),
            Ok(r#"{"directive": "add unit test", "continuity_brief": "continue", "planner_notes": "note1", "strategy_change": false}"#.into()),
            Ok(r#"{"score": 96, "satisfied": true, "evidence_quality": "verified", "critical_gaps": [], "iteration_summary": "step 2", "reason": "all verified"}"#.into()),
        ],
        ..MockHostState::default()
    });
    let service = Arc::new(LoopService::new());
    service.start(mock.clone()).await.unwrap();
    assert!(service.health().await.healthy);

    let driver = NativeLoopDriver::new(Arc::clone(&service));
    assert_eq!(driver.id(), LOOP_DRIVER_ID);

    let outcome = driver
        .run_loop(
            mock.clone(),
            LoopRunRequest {
                session,
                target: "ship feature".into(),
                budget: 5,
                max_wall_clock_secs: 60,
                satisfaction_threshold: 90,
                max_no_progress: 3,
                predicate: None,
                agent: Some(AgentName::new("hya-main")),
                model: Some(ModelRef::new("fake/worker")),
                config: json!({
                    "evaluator_model": "fake/judge",
                    "workdir": std::env::temp_dir(),
                }),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        DriverRunOutcome::Achieved {
            iterations: 2,
            reason: "satisfied: score 96".into(),
        }
    );
    assert_eq!(service.active_runs(), 0);
    assert_eq!(service.completed_runs(), 1);

    {
        let st = mock.state.lock().unwrap();
        assert_eq!(st.spawned_prompts.len(), 2);
        assert_eq!(st.spawned_prompts[0], "ship feature");
        assert!(st.spawned_prompts[1].contains("add unit test"));
        assert_eq!(st.model_requests.len(), 3);
        assert_eq!(
            st.model_requests[0].system.as_deref(),
            Some("CUSTOM VERIFIER PROMPT")
        );
        assert_eq!(
            st.model_requests[1].system.as_deref(),
            Some("CUSTOM PLANNER PROMPT")
        );
        assert_eq!(
            st.model_requests[0].model,
            Some(ModelRef::new("fake/judge"))
        );
        assert_eq!(st.events.len(), 2);
        assert_eq!(st.events[0].2, "loop.started");
        assert_eq!(st.events[1].2, "loop.finished");
    }

    service.stop().await.unwrap();
    assert!(!service.health().await.healthy);
}

#[tokio::test]
async fn native_loop_driver_supports_predicate_and_should_stop_and_rejects_bad_budget() {
    let service = Arc::new(LoopService::new());
    let driver = NativeLoopDriver::new(service);
    let session = SessionId::new();

    // 1. Out-of-range budget rejected up front.
    let mock = MockHostRuntime::new(MockHostState::default());
    let err = driver
        .run_loop(
            mock.clone(),
            LoopRunRequest {
                session,
                target: "t".into(),
                budget: 0,
                max_wall_clock_secs: 60,
                satisfaction_threshold: 90,
                max_no_progress: 3,
                predicate: None,
                agent: None,
                model: None,
                config: Value::Null,
            },
            CancellationToken::new(),
        )
        .await
        .expect_err("zero budget must fail");
    assert!(matches!(err, HostError::InvalidInput(_)));

    // 2. Deterministic predicate stops on first iteration without consulting model.
    let outcome = driver
        .run_loop(
            mock.clone(),
            LoopRunRequest {
                session,
                target: "reach target".into(),
                budget: 3,
                max_wall_clock_secs: 60,
                satisfaction_threshold: 90,
                max_no_progress: 3,
                predicate: Some(json!({
                    "command": "true",
                    "mode": "until",
                    "workdir": std::env::temp_dir(),
                })),
                agent: Some(AgentName::new("hya-main")),
                model: None,
                config: Value::Null,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    match outcome {
        DriverRunOutcome::Achieved { iterations, reason } => {
            assert_eq!(iterations, 1);
            assert!(reason.contains("until"), "{reason}");
        }
        other => panic!("expected achieved, got {other:?}"),
    }
    assert_eq!(mock.state.lock().unwrap().model_requests.len(), 0);

    // 3. `loop.should_stop` hook stops cleanly without consulting model.
    let hook_mock = MockHostRuntime::new(MockHostState {
        should_stop: Some("external gate says stop".into()),
        ..MockHostState::default()
    });
    let outcome = driver
        .run_loop(
            hook_mock.clone(),
            LoopRunRequest {
                session,
                target: "reach target".into(),
                budget: 3,
                max_wall_clock_secs: 60,
                satisfaction_threshold: 90,
                max_no_progress: 3,
                predicate: None,
                agent: Some(AgentName::new("hya-main")),
                model: None,
                config: Value::Null,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        DriverRunOutcome::Achieved {
            iterations: 1,
            reason: "loop.should_stop: external gate says stop".into(),
        }
    );
    assert_eq!(hook_mock.state.lock().unwrap().should_stop_calls, 1);
    assert_eq!(hook_mock.state.lock().unwrap().model_requests.len(), 0);
}

#[test]
fn abi_v2_symbol_reports_current_lockstep_digest() {
    let mut out = [0_u8; 32];
    // SAFETY: `out` is a writable 32-byte array.
    unsafe { hya_native_abi_v2(out.as_mut_ptr()) };
    assert_eq!(out, abi_digest_v2());
}
