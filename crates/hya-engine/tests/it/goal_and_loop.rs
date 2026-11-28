//! Integration tests for `hya-engine`: goal mode, loop mode, loop gate, and
//! deterministic loop-exit predicates.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use hya_engine::completion::{IterationDriver, IterationExecutor};
use hya_engine::hooks::{
    ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput, CommandExecuteBeforeOutcome,
    GoalEvaluateReply, HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome,
    NoopHookHost, TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome,
};
use hya_engine::loop_mode::{
    EvidenceQuality, HookLoopPlanner, HookLoopVerifier, LoopConfig, LoopGate, LoopPredicate,
    LoopPredicateOutcome, ModelLoopPlanner, ModelLoopVerifier, PlannerOutput, PredicateMode,
    VerifierVerdict, clamp_budget, validate_budget,
};
use hya_engine::{
    AgentSpec, CoreError, GateOutcome, IterationGate, LoopPlanner, LoopVerifier, RunOutcome,
    SafetyCaps, SessionEngine, drive_loop, run_loop,
};
use hya_proto::{Envelope, FinishReason, ModelRef, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_tool::PermissionRules;
use tokio_util::sync::CancellationToken;

fn working_provider() -> FakeProvider {
    FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("working".into()),
        FakeStep::Finish(FinishReason::Stop),
    ]])
}

fn router_with(provider: FakeProvider) -> Arc<ProviderRouter> {
    Arc::new(ProviderRouter::new().with(Arc::new(provider)))
}

fn caps_max(max_iterations: u32) -> SafetyCaps {
    SafetyCaps {
        max_iterations,
        ..SafetyCaps::default()
    }
}

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

async fn engine_and_session(
    provider: FakeProvider,
    agent_name: &str,
) -> (Arc<SessionEngine>, SessionId, AgentSpec) {
    let eng = Arc::new(
        support::engine_with_provider(Arc::new(provider), PermissionRules::default()).await,
    );
    let mut agent = support::agent_spec(agent_name, "fake", "/tmp");
    agent.system_prompt = "x".into();
    let session = support::create_root_session(&eng, agent_name, "fake", Path::new("/tmp")).await;
    (eng, session, agent)
}

#[derive(Default)]
struct TestHooks {
    goal_replies: Vec<GoalEvaluateReply>,
    goal_idx: AtomicUsize,
    should_stop: Option<String>,
    should_stop_calls: AtomicUsize,
    loop_eval: bool,
}

#[async_trait]
impl HookDispatcher for TestHooks {
    fn dispatch_event(&self, _: &Envelope) {}
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
    async fn goal_evaluate(&self, _: &str, _: &str) -> Result<GoalEvaluateReply, CoreError> {
        let i = self.goal_idx.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .goal_replies
            .get(i)
            .cloned()
            .unwrap_or(GoalEvaluateReply::Verdict {
                met: true,
                reason: "scripted default".into(),
            }))
    }
    async fn loop_should_stop(&self, _: &str, _: &str) -> Option<String> {
        self.should_stop_calls.fetch_add(1, Ordering::SeqCst);
        self.should_stop.clone()
    }
    async fn loop_verify(&self, t: &str, tr: &str) -> Result<VerifierVerdict, CoreError> {
        if self.loop_eval {
            Ok(VerifierVerdict {
                score: 91,
                satisfied: true,
                evidence_quality: EvidenceQuality::Verified,
                critical_gaps: vec![],
                iteration_summary: "hook".into(),
                reason: "hook verdict".into(),
            })
        } else {
            NoopHookHost.loop_verify(t, tr).await
        }
    }
    async fn loop_plan(
        &self,
        t: &str,
        h: &[String],
        l: &VerifierVerdict,
        n: &str,
    ) -> Result<PlannerOutput, CoreError> {
        if self.loop_eval {
            Ok(PlannerOutput {
                directive: "hook directive".into(),
                continuity_brief: "hook".into(),
                planner_notes: String::new(),
                strategy_change: false,
                change_note: String::new(),
            })
        } else {
            NoopHookHost.loop_plan(t, h, l, n).await
        }
    }
}

struct FailingProvider;

#[async_trait]
impl Provider for FailingProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities::default())
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        _: SessionId,
        _: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(stream::iter(vec![Err(ProviderError::Http(
            "boom".into(),
        ))])))
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
    async fn grade(&self, _: &str, _: &str) -> Result<VerifierVerdict, CoreError> {
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
    ) -> Result<PlannerOutput, CoreError> {
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
    async fn run_iteration(&self, _: &str, _: &CancellationToken) -> Result<String, CoreError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(String::new())
    }
}

async fn drive_case(
    verdicts: Vec<VerifierVerdict>,
    dirs: Vec<(&str, bool)>,
    config: LoopConfig,
) -> (RunOutcome, usize, usize, usize) {
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

struct WrapFailure {
    calls: AtomicUsize,
    cancel: bool,
}

#[async_trait]
impl IterationExecutor for WrapFailure {
    async fn run_iteration(&self, _: &str, token: &CancellationToken) -> Result<String, CoreError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok("latest worker evidence".into());
        }
        if self.cancel {
            token.cancel();
            return Err(CoreError::Cancelled);
        }
        Err(CoreError::CapacityExceeded {
            requested: 1,
            limit: 1,
        })
    }
}

struct ContinueGate;

#[async_trait]
impl IterationGate for ContinueGate {
    async fn judge(&self, _: &str, _: &CancellationToken) -> Result<GateOutcome, CoreError> {
        Ok(GateOutcome::Continue {
            directive: "continue".into(),
        })
    }
}

fn gate_with_hook(
    verifier: Arc<ScriptedLoop>,
    hook: Option<Arc<dyn HookDispatcher>>,
    pred: Option<LoopPredicate>,
) -> LoopGate {
    LoopGate::with_should_stop_hook(
        LoopGate::new(
            "target".into(),
            verifier,
            ScriptedLoop::constant(false),
            LoopConfig {
                predicate: pred,
                ..LoopConfig::default()
            },
        ),
        hook,
    )
}

// ---------------------------------------------------------------------------
// 1. IterationDriver wrap-up failure & cancellation (goal tests live in hya/goal)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn goal_wrap_up_failure_propagates_capacity_and_preserves_cancellation() {
    let cap_res = IterationDriver::new(caps_max(1))
        .run(
            &WrapFailure {
                calls: AtomicUsize::new(0),
                cancel: false,
            },
            &ContinueGate,
            "work".into(),
            CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(cap_res, Err(CoreError::CapacityExceeded { .. })),
        "{cap_res:?}"
    );

    let cancel_res = IterationDriver::new(caps_max(1))
        .run(
            &WrapFailure {
                calls: AtomicUsize::new(0),
                cancel: true,
            },
            &ContinueGate,
            "work".into(),
            CancellationToken::new(),
        )
        .await;
    assert!(
        matches!(cancel_res, Ok(RunOutcome::Cancelled)),
        "{cancel_res:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. Loop mode: model verifier/planner parsing, hook priority, should_stop, run_loop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn model_loop_verifier_and_planner_parsing_and_degradation_matrix() {
    let model = ModelRef::new("fake/verifier");
    struct VerifierCase {
        router: Arc<ProviderRouter>,
        satisfied: bool,
        score: u8,
        quality: EvidenceQuality,
        gaps: &'static [&'static str],
        summary: &'static str,
        reason_needle: &'static str,
    }
    let cases = [
        VerifierCase {
            router: router_with(FakeProvider::scripted(vec![FakeStep::Text(r#"{"score": 87, "satisfied": true, "evidence_quality": "verified", "critical_gaps": [], "iteration_summary": "tests green", "reason": "all criteria met"}"#.into())])),
            satisfied: true,
            score: 87,
            quality: EvidenceQuality::Verified,
            gaps: &[],
            summary: "tests green",
            reason_needle: "all criteria met",
        },
        VerifierCase {
            router: router_with(FakeProvider::scripted(vec![FakeStep::Text("```json\n{\"score\": 40, \"satisfied\": false, \"evidence_quality\": \"claim_only\", \"critical_gaps\": [\"no tests\"], \"reason\": \"unfinished\"}\n```".into())])),
            satisfied: false,
            score: 40,
            quality: EvidenceQuality::ClaimOnly,
            gaps: &["no tests"],
            summary: "",
            reason_needle: "unfinished",
        },
        VerifierCase {
            router: router_with(FakeProvider::scripted(vec![FakeStep::Text("I think it looks done!".into())])),
            satisfied: false,
            score: 0,
            quality: EvidenceQuality::Missing,
            gaps: &[],
            summary: "",
            reason_needle: "verifier returned malformed output",
        },
        VerifierCase {
            router: Arc::new(ProviderRouter::new().with(Arc::new(FailingProvider))),
            satisfied: false,
            score: 0,
            quality: EvidenceQuality::Missing,
            gaps: &[],
            summary: "",
            reason_needle: "verifier provider failed",
        },
    ];
    for c in cases {
        let got = ModelLoopVerifier::new(c.router, model.clone())
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
    let good = router_with(FakeProvider::scripted(vec![FakeStep::Text(r#"{"directive": "write the failing test", "continuity_brief": "branch is clean", "planner_notes": "focus on parser", "strategy_change": true}"#.into())]));
    let plan = ModelLoopPlanner::new(good, model.clone())
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

    let bad = router_with(FakeProvider::scripted(vec![FakeStep::Text(
        "just keep going".into(),
    )]));
    let fb = ModelLoopPlanner::new(bad, model)
        .plan_next("target", &[], &v, "prior notes")
        .await
        .unwrap();
    assert!(!fb.directive.is_empty() && !fb.strategy_change && fb.planner_notes.is_empty());
}

#[tokio::test]
async fn process_loop_evaluators_take_priority_and_failures_use_fallback() {
    let hook = Arc::new(TestHooks {
        loop_eval: true,
        ..TestHooks::default()
    });
    let unused = ScriptedLoop::constant(false);
    let v = HookLoopVerifier::new(hook.clone(), unused.clone())
        .grade("target", "transcript")
        .await
        .unwrap();
    assert_eq!(
        (v.reason.as_str(), unused.v_calls.load(Ordering::SeqCst)),
        ("hook verdict", 0)
    );

    let plan = HookLoopPlanner::new(hook, unused)
        .plan_next("target", &[], &v, "")
        .await
        .unwrap();
    assert_eq!(plan.directive, "hook directive");

    let fb = ScriptedLoop::constant(true);
    assert!(
        HookLoopVerifier::new(Arc::new(NoopHookHost), fb.clone())
            .grade("target", "transcript")
            .await
            .unwrap()
            .satisfied
    );
    assert_eq!(fb.v_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn loop_gate_should_stop_hook_and_broken_predicate_precedence() {
    let v1 = ScriptedLoop::constant(true);
    let h1 = Arc::new(TestHooks {
        should_stop: Some("worker says done".into()),
        ..TestHooks::default()
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
            h1.should_stop_calls.load(Ordering::SeqCst),
            v1.v_calls.load(Ordering::SeqCst)
        ),
        (1, 0)
    );

    for hook in [
        Some(Arc::new(TestHooks::default()) as Arc<dyn HookDispatcher>),
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

    let hook = Arc::new(TestHooks {
        should_stop: Some("worker says done".into()),
        ..TestHooks::default()
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
async fn run_loop_end_to_end_predicate_and_should_stop_hook() {
    let (eng, sid, agent) = engine_and_session(working_provider(), "hya-main").await;
    let verifier = ScriptedLoop::constant(true);
    let cfg = LoopConfig {
        predicate: Some(LoopPredicate::new(
            "true",
            PredicateMode::Until,
            PathBuf::from("/tmp"),
        )),
        ..LoopConfig::default()
    };
    let out = run_loop(
        eng,
        sid,
        agent,
        "reach the target".into(),
        verifier.clone(),
        ScriptedLoop::constant(false),
        cfg,
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();
    match out {
        RunOutcome::Achieved { iterations, reason } => assert!(
            iterations == 1 && reason.contains("until"),
            "{iterations}: {reason}"
        ),
        other => panic!("expected achieved, got {other:?}"),
    }
    assert_eq!(verifier.v_calls.load(Ordering::SeqCst), 0);

    let (eng, sid, agent) = engine_and_session(working_provider(), "hya-main").await;
    let hook = Arc::new(TestHooks {
        should_stop: Some("external gate says stop".into()),
        ..TestHooks::default()
    });
    let out = run_loop(
        eng,
        sid,
        agent,
        "reach the target".into(),
        ScriptedLoop::constant(false),
        ScriptedLoop::constant(false),
        LoopConfig::default(),
        CancellationToken::new(),
        Some(hook),
    )
    .await
    .unwrap();
    assert_eq!(
        out,
        RunOutcome::Achieved {
            iterations: 1,
            reason: "loop.should_stop: external gate says stop".into()
        }
    );
}

// ---------------------------------------------------------------------------
// 3. Loop gate: iteration progression, budgets, stall guards, cancellation
// ---------------------------------------------------------------------------

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
    assert!(matches!(out, RunOutcome::Achieved { iterations: 3, .. }));
    assert_eq!((vc, pc, ec), (3, 2, 3));

    let (imm, _, pc2, _) = drive_case(
        vec![verdict(95, true, &[])],
        vec![("d", false)],
        LoopConfig::default(),
    )
    .await;
    assert!(matches!(imm, RunOutcome::Achieved { iterations: 1, .. }));
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
        RunOutcome::BudgetLimited {
            iterations: 3,
            which: "max_iterations"
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
        RunOutcome::BudgetLimited {
            iterations: 3,
            which: "max_iterations"
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
    assert_eq!(clamp_budget(500), clamp_budget(usize::MAX as u32));
    assert!(
        validate_budget(&LoopConfig {
            budget: clamp_budget(500),
            ..LoopConfig::default()
        })
        .is_ok()
    );
}

#[tokio::test]
async fn loop_gate_stall_guards_detect_no_progress_and_repeated_directive() {
    let (np, _, _, _) = drive_case(
        vec![verdict(50, false, &["x"])],
        vec![("d1", false), ("d2", false)],
        LoopConfig::default(),
    )
    .await;
    assert_eq!(np, RunOutcome::Stalled { iterations: 3 });

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
    assert_eq!(rep, RunOutcome::Stalled { iterations: 2 });
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
    assert_eq!(outcome.unwrap(), RunOutcome::Cancelled);
}

// ---------------------------------------------------------------------------
// 4. Deterministic loop-exit predicates (--until / --while / broken / timeout)
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
