//! Deterministic loop-exit predicates (`--while` / `--until`).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use hya_core::loop_mode::{
    LoopConfig, LoopGate, LoopPlanner, LoopPredicate, LoopPredicateOutcome, LoopVerifier,
    PlannerOutput, PredicateMode, VerifierVerdict,
};
use hya_core::{CoreError, GateOutcome, IterationGate};

struct ScriptedVerifier {
    satisfied: bool,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LoopVerifier for ScriptedVerifier {
    async fn grade(&self, _target: &str, _transcript: &str) -> Result<VerifierVerdict, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VerifierVerdict {
            score: 100,
            satisfied: self.satisfied,
            evidence_quality: hya_core::loop_mode::EvidenceQuality::Verified,
            critical_gaps: Vec::new(),
            iteration_summary: "scripted".to_string(),
            reason: "scripted".to_string(),
        })
    }
}

struct NoopPlanner;

#[async_trait::async_trait]
impl LoopPlanner for NoopPlanner {
    async fn plan_next(
        &self,
        _target: &str,
        _history: &[String],
        _last_verdict: &VerifierVerdict,
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

fn predicate(command: &str, mode: PredicateMode) -> LoopPredicate {
    LoopPredicate {
        command: command.to_string(),
        mode,
        timeout: Duration::from_secs(5),
        workdir: std::env::temp_dir(),
    }
}

fn gate(command: &str, mode: PredicateMode, verifier_satisfied: bool) -> LoopGate {
    LoopGate::new(
        "target".to_string(),
        Arc::new(ScriptedVerifier {
            satisfied: verifier_satisfied,
            calls: AtomicUsize::new(0),
        }),
        Arc::new(NoopPlanner),
        LoopConfig {
            predicate: Some(predicate(command, mode)),
            ..LoopConfig::default()
        },
    )
}

#[tokio::test]
async fn until_exit_zero_stops_even_when_verifier_agrees() {
    let gate = gate("true", PredicateMode::Until, true);
    match gate.judge("transcript").await.unwrap() {
        GateOutcome::Stop { reason } => {
            assert!(reason.contains("until"), "reason: {reason}");
        }
        _other => panic!("expected stop"),
    }
}

#[tokio::test]
async fn until_exit_one_continues_despite_satisfied_verifier() {
    // The predicate outranks the model verdict: `until` with exit 1 keeps the
    // loop going even when the verifier would declare success.
    let gate = gate("false", PredicateMode::Until, true);
    match gate.judge("transcript").await.unwrap() {
        GateOutcome::Continue { .. } => {}
        _other => panic!("expected continue"),
    }
}

#[tokio::test]
async fn while_exit_one_stops() {
    let gate = gate("false", PredicateMode::While, true);
    match gate.judge("transcript").await.unwrap() {
        GateOutcome::Stop { reason } => {
            assert!(reason.contains("while"), "reason: {reason}");
        }
        _other => panic!("expected stop"),
    }
}

#[tokio::test]
async fn broken_condition_stops_without_success() {
    let gate = gate("exit 127", PredicateMode::Until, false);
    match gate.judge("transcript").await.unwrap() {
        GateOutcome::Stop { reason } => {
            assert!(reason.contains("broken condition"), "reason: {reason}");
        }
        _other => panic!("expected stop"),
    }
    assert!(
        gate.broken_condition().is_some(),
        "broken state must be observable for the entry point"
    );
}

#[tokio::test]
async fn condition_timeout_is_broken_not_continue() {
    let mut pred = predicate("sleep 5", PredicateMode::Until);
    pred.timeout = Duration::from_millis(100);
    let gate = LoopGate::new(
        "target".to_string(),
        Arc::new(ScriptedVerifier {
            satisfied: false,
            calls: AtomicUsize::new(0),
        }),
        Arc::new(NoopPlanner),
        LoopConfig {
            predicate: Some(pred),
            ..LoopConfig::default()
        },
    );
    match gate.judge("transcript").await.unwrap() {
        GateOutcome::Stop { reason } => {
            assert!(reason.contains("broken condition"), "reason: {reason}");
        }
        _other => panic!("expected stop"),
    }
}

#[test]
fn predicate_evaluation_unit_levels() {
    let pred = predicate("true", PredicateMode::Until);
    assert!(matches!(
        pred.evaluate(),
        hya_core::loop_mode::LoopPredicateOutcome::Satisfied { .. }
    ));
    let pred = predicate("false", PredicateMode::Until);
    assert!(matches!(
        pred.evaluate(),
        hya_core::loop_mode::LoopPredicateOutcome::Continue
    ));
    let pred = predicate("exit 42", PredicateMode::Until);
    match pred.evaluate() {
        hya_core::loop_mode::LoopPredicateOutcome::Broken { detail } => {
            assert!(detail.contains("42"), "detail: {detail}");
        }
        other => panic!("expected broken, got {other:?}"),
    }
}
