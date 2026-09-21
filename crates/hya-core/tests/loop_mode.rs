//! Loop-mode model plumbing (dev_plan 6.11 + 6.5): `ModelLoopVerifier` /
//! `ModelLoopPlanner` tolerance contracts, the `loop.should_stop` gate
//! ordering, `clamp_budget`, and a scripted `run_loop` with a predicate.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::stream;
use hya_core::hooks::{
    ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput, CommandExecuteBeforeOutcome,
    HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome, NoopHookHost,
    TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput, ToolExecuteAfterOutcome,
    ToolExecuteBeforeInput, ToolExecuteBeforeOutcome,
};
use hya_core::loop_mode::{
    EvidenceQuality, LoopConfig, LoopGate, LoopPredicate, ModelLoopPlanner, ModelLoopVerifier,
    PlannerOutput, PredicateMode, VerifierVerdict, clamp_budget, cost_preflight,
};
use hya_core::{
    AgentSpec, CoreError, CreateSession, GateOutcome, IterationGate, LoopPlanner, LoopVerifier,
    RunOutcome, run_loop,
};
use hya_proto::{AgentName, Envelope, FinishReason, ModelRef};
use hya_provider::{
    CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use tokio_util::sync::CancellationToken;

fn router_with(provider: FakeProvider) -> Arc<ProviderRouter> {
    Arc::new(ProviderRouter::new().with(Arc::new(provider)))
}

/// Provider whose stream always fails: models the "verifier provider is down"
/// case that must degrade to not-satisfied instead of aborting the run.
struct FailingProvider;

#[async_trait]
impl Provider for FailingProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<hya_provider::Capabilities> {
        Some(hya_provider::Capabilities::default())
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        _session: hya_proto::SessionId,
        _message: hya_proto::MessageId,
    ) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(stream::iter(vec![Err(ProviderError::Http(
            "boom".to_string(),
        ))])))
    }
}

fn verifier_model() -> ModelRef {
    ModelRef::new("fake/verifier")
}

fn transcript(text: &str) -> String {
    format!("[User] do it\n[Assistant] {text}")
}

#[tokio::test]
async fn model_loop_verifier_parses_well_formed_json() {
    let router = router_with(FakeProvider::scripted(vec![FakeStep::Text(
        r#"{"score": 87, "satisfied": true, "evidence_quality": "verified",
            "critical_gaps": [], "iteration_summary": "tests green",
            "reason": "all criteria met"}"#
            .to_string(),
    )]));
    let verdict = ModelLoopVerifier::new(router, verifier_model())
        .grade("target", &transcript("work"))
        .await
        .unwrap();
    assert!(verdict.satisfied);
    assert_eq!(verdict.score, 87);
    assert_eq!(verdict.evidence_quality, EvidenceQuality::Verified);
    assert!(verdict.critical_gaps.is_empty());
    assert_eq!(verdict.iteration_summary, "tests green");
    assert_eq!(verdict.reason, "all criteria met");
}

/// Tolerant parse: fenced or prose-wrapped JSON objects are still extracted
/// (mirrors `parse_verdict` in workflow/run.rs).
#[tokio::test]
async fn model_loop_verifier_extracts_json_from_fenced_reply() {
    let router = router_with(FakeProvider::scripted(vec![FakeStep::Text(
        "```json\n{\"score\": 40, \"satisfied\": false,\n  \"evidence_quality\": \"claim_only\",\n  \"critical_gaps\": [\"no tests\"],\n  \"reason\": \"unfinished\"}\n```"
            .to_string(),
    )]));
    let verdict = ModelLoopVerifier::new(router, verifier_model())
        .grade("target", &transcript("work"))
        .await
        .unwrap();
    assert!(!verdict.satisfied);
    assert_eq!(verdict.score, 40);
    assert_eq!(verdict.evidence_quality, EvidenceQuality::ClaimOnly);
    assert_eq!(verdict.critical_gaps, vec!["no tests".to_string()]);
}

/// Malformed output degrades to not-satisfied with score 0 and Missing
/// evidence: a broken verdict counts against the loop instead of erroring it.
#[tokio::test]
async fn model_loop_verifier_malformed_output_is_not_satisfied() {
    let router = router_with(FakeProvider::scripted(vec![FakeStep::Text(
        "I think it looks done!".to_string(),
    )]));
    let verdict = ModelLoopVerifier::new(router, verifier_model())
        .grade("target", &transcript("work"))
        .await
        .unwrap();
    assert!(!verdict.satisfied, "malformed must read as not satisfied");
    assert_eq!(verdict.score, 0);
    assert_eq!(verdict.evidence_quality, EvidenceQuality::Missing);
    assert_eq!(
        verdict.reason, "verifier returned malformed output",
        "the documented malformed reason must be stable"
    );
}

/// A failing provider chain degrades to not-satisfied too: the loop stays
/// cappable instead of aborting on the first verifier hiccup.
#[tokio::test]
async fn model_loop_verifier_provider_error_is_not_satisfied() {
    let router = router_with_failing();
    let verdict = ModelLoopVerifier::new(router, verifier_model())
        .grade("target", &transcript("work"))
        .await
        .unwrap();
    assert!(!verdict.satisfied, "provider errors must not satisfy");
    assert_eq!(verdict.score, 0);
    assert_eq!(verdict.evidence_quality, EvidenceQuality::Missing);
    assert!(
        verdict.reason.contains("verifier provider failed"),
        "reason: {}",
        verdict.reason
    );
}

fn router_with_failing() -> Arc<ProviderRouter> {
    Arc::new(ProviderRouter::new().with(Arc::new(FailingProvider)))
}

#[tokio::test]
async fn model_loop_planner_parses_well_formed_json() {
    let router = router_with(FakeProvider::scripted(vec![FakeStep::Text(
        r#"{"directive": "write the failing test", "continuity_brief": "branch is clean",
            "planner_notes": "focus on parser", "strategy_change": true}"#
            .to_string(),
    )]));
    let verdict = not_satisfied_verdict();
    let plan = ModelLoopPlanner::new(router, verifier_model())
        .plan_next("target", &[], &verdict, "")
        .await
        .unwrap();
    assert_eq!(plan.directive, "write the failing test");
    assert_eq!(plan.continuity_brief, "branch is clean");
    assert_eq!(plan.planner_notes, "focus on parser");
    assert!(plan.strategy_change);
}

/// Malformed planner output degrades to a neutral continue directive: the
/// loop keeps making progress instead of aborting or repeating a broken plan.
#[tokio::test]
async fn model_loop_planner_malformed_output_is_neutral_continue() {
    let router = router_with(FakeProvider::scripted(vec![FakeStep::Text(
        "just keep going".to_string(),
    )]));
    let verdict = not_satisfied_verdict();
    let plan = ModelLoopPlanner::new(router, verifier_model())
        .plan_next("target", &[], &verdict, "prior notes")
        .await
        .unwrap();
    assert!(
        !plan.directive.is_empty(),
        "the neutral fallback must still direct the worker"
    );
    assert!(
        !plan.strategy_change,
        "the fallback must not claim a strategy change"
    );
    assert!(
        plan.planner_notes.is_empty(),
        "a malformed reply must not invent planner notes"
    );
}

fn not_satisfied_verdict() -> VerifierVerdict {
    VerifierVerdict {
        score: 10,
        satisfied: false,
        evidence_quality: EvidenceQuality::Missing,
        critical_gaps: vec!["everything".to_string()],
        iteration_summary: "started".to_string(),
        reason: "not yet".to_string(),
    }
}

/// Engine-supplied post-turn stop consult: `Some(reason)` stops with a
/// `loop.should_stop:`-prefixed reason, and is a legitimate stop (recorded as
/// `Achieved`); `None` falls through untouched.
struct StopHook {
    reply: Option<String>,
    calls: AtomicUsize,
}

#[async_trait]
impl HookDispatcher for StopHook {
    fn dispatch_event(&self, _envelope: &Envelope) {}

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        NoopHookHost.command_execute_before(input).await
    }

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        NoopHookHost.text_complete(input).await
    }

    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        NoopHookHost.message_user_before(input).await
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        NoopHookHost.chat_params(input).await
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        NoopHookHost.tool_execute_before(input).await
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        NoopHookHost.tool_execute_after(input).await
    }

    async fn loop_should_stop(&self, _target: &str, _transcript: &str) -> Option<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.reply.clone()
    }
}

struct CountingVerifier {
    satisfied: bool,
    calls: AtomicUsize,
}

#[async_trait]
impl LoopVerifier for CountingVerifier {
    async fn grade(&self, _target: &str, _transcript: &str) -> Result<VerifierVerdict, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VerifierVerdict {
            score: 100,
            satisfied: self.satisfied,
            evidence_quality: EvidenceQuality::Verified,
            critical_gaps: Vec::new(),
            iteration_summary: "scripted".to_string(),
            reason: "scripted".to_string(),
        })
    }
}

#[async_trait]
impl LoopPlanner for CountingVerifier {
    async fn plan_next(
        &self,
        _target: &str,
        _history: &[String],
        _last: &VerifierVerdict,
        _planner_notes: &str,
    ) -> Result<PlannerOutput, CoreError> {
        Ok(PlannerOutput {
            directive: "continue".to_string(),
            continuity_brief: String::new(),
            planner_notes: String::new(),
            strategy_change: false,
            change_note: String::new(),
        })
    }
}

fn counting_verifier(satisfied: bool) -> Arc<CountingVerifier> {
    Arc::new(CountingVerifier {
        satisfied,
        calls: AtomicUsize::new(0),
    })
}

fn gate_with_hook(
    verifier: Arc<CountingVerifier>,
    hook: Option<Arc<dyn HookDispatcher>>,
) -> LoopGate {
    LoopGate::with_should_stop_hook(
        LoopGate::new(
            "target".to_string(),
            verifier,
            Arc::new(CountingVerifier {
                satisfied: false,
                calls: AtomicUsize::new(0),
            }),
            LoopConfig::default(),
        ),
        hook,
    )
}

/// Ordering: `loop.should_stop` fires after the predicate and before the
/// verifier — a `Some(reason)` stops the loop even when the verifier would
/// declare success.
#[tokio::test]
async fn should_stop_hook_stops_before_verifier_satisfied_check() {
    let verifier = counting_verifier(true);
    let hook = Arc::new(StopHook {
        reply: Some("worker says done".to_string()),
        calls: AtomicUsize::new(0),
    });
    let gate = gate_with_hook(verifier.clone(), Some(hook.clone()));
    match gate.judge(&transcript("work")).await.unwrap() {
        GateOutcome::Stop { reason } => {
            assert_eq!(
                reason, "loop.should_stop: worker says done",
                "the hook stop must carry the documented prefix"
            );
        }
        GateOutcome::Continue { .. } => panic!("expected a stop, got continue"),
    }
    assert_eq!(
        hook.calls.load(Ordering::SeqCst),
        1,
        "the hook must be consulted once per judgment"
    );
    assert_eq!(
        verifier.calls.load(Ordering::SeqCst),
        0,
        "the hook fires before the verifier is consulted at all"
    );
}

/// A `None` reply keeps the pipeline intact: the verifier satisfied check
/// still stops the loop as before.
#[tokio::test]
async fn should_stop_hook_none_falls_through_to_verifier() {
    let verifier = counting_verifier(true);
    let hook = Arc::new(StopHook {
        reply: None,
        calls: AtomicUsize::new(0),
    });
    let gate = gate_with_hook(verifier.clone(), Some(hook));
    match gate.judge(&transcript("work")).await.unwrap() {
        GateOutcome::Stop { reason } => {
            assert_eq!(reason, "satisfied: score 100");
        }
        GateOutcome::Continue { .. } => panic!("expected the satisfied stop, got continue"),
    }
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);
}

/// No hook wired at all: behavior is unchanged (verifier decides).
#[tokio::test]
async fn gate_without_hook_stops_on_satisfied_verifier() {
    let verifier = counting_verifier(true);
    let gate = gate_with_hook(verifier.clone(), None);
    match gate.judge(&transcript("work")).await.unwrap() {
        GateOutcome::Stop { reason } => {
            assert_eq!(reason, "satisfied: score 100");
        }
        GateOutcome::Continue { .. } => panic!("expected the satisfied stop, got continue"),
    }
}

/// Ordering: a broken deterministic predicate outranks the hook — the run
/// stops as a failure (never success) even when the hook asks to stop, and
/// the broken detail stays observable via `LoopGate::broken_condition`.
#[tokio::test]
async fn broken_predicate_wins_over_should_stop_hook() {
    let hook = Arc::new(StopHook {
        reply: Some("worker says done".to_string()),
        calls: AtomicUsize::new(0),
    });
    let verifier = counting_verifier(false);
    let gate = LoopGate::with_should_stop_hook(
        LoopGate::new(
            "target".to_string(),
            verifier,
            Arc::new(CountingVerifier {
                satisfied: false,
                calls: AtomicUsize::new(0),
            }),
            LoopConfig {
                predicate: Some(LoopPredicate::new(
                    "exit 127",
                    PredicateMode::Until,
                    std::env::temp_dir(),
                )),
                ..LoopConfig::default()
            },
        ),
        Some(hook),
    );
    match gate.judge(&transcript("work")).await.unwrap() {
        GateOutcome::Stop { reason } => {
            assert!(
                reason.starts_with("broken condition"),
                "broken condition must win and read as a failure: {reason}"
            );
        }
        GateOutcome::Continue { .. } => {
            panic!("expected a broken-condition stop, got continue")
        }
    }
    assert!(
        gate.broken_condition().is_some(),
        "the broken detail must stay observable on the gate"
    );
}

/// `clamp_budget` maps any requested budget into the range `cost_preflight`
/// enforces (1..=HARD_MAX_ITERATIONS), and the clamped value passes preflight.
#[test]
fn clamp_budget_maps_into_preflight_range() {
    assert_eq!(clamp_budget(0), 1, "zero budget clamps to the minimum");
    assert_eq!(clamp_budget(7), 7, "in-range budgets pass through");
    assert_eq!(
        clamp_budget(500),
        clamp_budget(usize::MAX as u32),
        "over-ceiling budgets clamp to the same hard max"
    );
    let config = LoopConfig {
        budget: clamp_budget(500),
        ..LoopConfig::default()
    };
    assert!(
        cost_preflight(&config).is_ok(),
        "a clamped budget must always pass cost_preflight"
    );
}

async fn engine_with(provider: FakeProvider) -> (Arc<hya_core::SessionEngine>, AgentSpec) {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(hya_core::SessionEngine::new(
        store,
        router,
        support::test_runtime(tools),
        perm,
        hya_core::EventBus::default(),
    ));
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent)
}

/// Scripted end-to-end: `run_loop` with a deterministic `until` predicate
/// stops on the first judgment with the predicate reason — and without the
/// verifier ever being consulted.
#[tokio::test]
async fn run_loop_with_predicate_stops_on_first_judgment() {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("working".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let (engine, agent) = engine_with(provider).await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let verifier = counting_verifier(true);

    let outcome = run_loop(
        engine,
        session,
        agent,
        "reach the target".to_string(),
        verifier.clone(),
        Arc::new(CountingVerifier {
            satisfied: false,
            calls: AtomicUsize::new(0),
        }),
        LoopConfig {
            predicate: Some(LoopPredicate::new(
                "true",
                PredicateMode::Until,
                PathBuf::from("/tmp"),
            )),
            ..LoopConfig::default()
        },
        CancellationToken::new(),
        None,
    )
    .await
    .unwrap();

    assert!(matches!(
        outcome,
        RunOutcome::Achieved { iterations: 1, .. }
    ));
    if let RunOutcome::Achieved { reason, .. } = outcome {
        assert!(reason.contains("until"), "reason: {reason}");
    }
    assert_eq!(
        verifier.calls.load(Ordering::SeqCst),
        0,
        "a satisfied predicate stops before the verifier runs"
    );
}

/// Scripted end-to-end: an engine-supplied `loop.should_stop` hook stops the
/// run after the first iteration with the prefixed reason (a legitimate stop,
/// recorded as `Achieved`).
#[tokio::test]
async fn run_loop_with_should_stop_hook_stops_after_first_iteration() {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("working".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let (engine, agent) = engine_with(provider).await;
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();

    let outcome = run_loop(
        engine,
        session,
        agent,
        "reach the target".to_string(),
        counting_verifier(false),
        Arc::new(CountingVerifier {
            satisfied: false,
            calls: AtomicUsize::new(0),
        }),
        LoopConfig::default(),
        CancellationToken::new(),
        Some(Arc::new(StopHook {
            reply: Some("external gate says stop".to_string()),
            calls: AtomicUsize::new(0),
        })),
    )
    .await
    .unwrap();

    match outcome {
        RunOutcome::Achieved { iterations, reason } => {
            assert_eq!(iterations, 1);
            assert_eq!(reason, "loop.should_stop: external gate says stop");
        }
        other => panic!("expected an achieved stop, got {other:?}"),
    }
}
