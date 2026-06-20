#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use yaca_core::completion::IterationExecutor;
use yaca_core::{
    CoreError, EvidenceQuality, LoopConfig, LoopPlanner, LoopVerifier, PlannerOutput, RunOutcome,
    VerifierVerdict, cost_preflight, drive_loop,
};

fn verdict(score: u8, satisfied: bool, gaps: &[&str]) -> VerifierVerdict {
    VerifierVerdict {
        score,
        satisfied,
        evidence_quality: if satisfied {
            EvidenceQuality::Verified
        } else {
            EvidenceQuality::ClaimOnly
        },
        critical_gaps: gaps.iter().map(|s| (*s).to_string()).collect(),
        iteration_summary: format!("score {score}"),
        reason: "r".to_string(),
    }
}

struct ScriptedVerifier {
    verdicts: Vec<VerifierVerdict>,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl LoopVerifier for ScriptedVerifier {
    async fn grade(&self, _target: &str, _transcript: &str) -> Result<VerifierVerdict, CoreError> {
        let i = self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .verdicts
            .get(i)
            .or_else(|| self.verdicts.last())
            .cloned()
            .unwrap())
    }
}

struct ScriptedPlanner {
    dirs: Vec<(String, bool)>,
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl LoopPlanner for ScriptedPlanner {
    async fn plan_next(
        &self,
        _target: &str,
        _history: &[String],
        _last: &VerifierVerdict,
        _notes: &str,
    ) -> Result<PlannerOutput, CoreError> {
        let i = self.calls.fetch_add(1, Ordering::Relaxed);
        let (directive, strategy_change) = self
            .dirs
            .get(i)
            .or_else(|| self.dirs.last())
            .cloned()
            .unwrap();
        Ok(PlannerOutput {
            directive,
            continuity_brief: "brief".to_string(),
            planner_notes: "notes".to_string(),
            strategy_change,
            change_note: String::new(),
        })
    }
}

struct CountingExecutor {
    calls: Arc<AtomicUsize>,
}
#[async_trait]
impl IterationExecutor for CountingExecutor {
    async fn run_iteration(
        &self,
        _directive: &str,
        _cancel: &CancellationToken,
    ) -> Result<String, CoreError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(String::new())
    }
}

fn counters() -> (Arc<AtomicUsize>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
}

#[tokio::test]
async fn loops_until_verifier_satisfied() {
    let (vc, pc, ec) = counters();
    let verifier = Arc::new(ScriptedVerifier {
        verdicts: vec![
            verdict(40, false, &["a"]),
            verdict(76, false, &["b"]),
            verdict(93, true, &[]),
        ],
        calls: vc.clone(),
    });
    let planner = Arc::new(ScriptedPlanner {
        dirs: vec![
            ("d1".into(), false),
            ("d2".into(), false),
            ("d3".into(), false),
        ],
        calls: pc.clone(),
    });
    let executor = CountingExecutor { calls: ec.clone() };
    let outcome = drive_loop(
        &executor,
        verifier,
        planner,
        "target".to_string(),
        LoopConfig::default(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Achieved { iterations: 3, .. }
    ));
    assert_eq!(vc.load(Ordering::Relaxed), 3);
    assert_eq!(pc.load(Ordering::Relaxed), 2);
    assert_eq!(ec.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn planner_skipped_on_terminal_success() {
    let (vc, pc, ec) = counters();
    let verifier = Arc::new(ScriptedVerifier {
        verdicts: vec![verdict(95, true, &[])],
        calls: vc.clone(),
    });
    let planner = Arc::new(ScriptedPlanner {
        dirs: vec![("d".into(), false)],
        calls: pc.clone(),
    });
    let outcome = drive_loop(
        &CountingExecutor { calls: ec.clone() },
        verifier,
        planner,
        "t".to_string(),
        LoopConfig::default(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome,
        RunOutcome::Achieved { iterations: 1, .. }
    ));
    assert_eq!(pc.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn budget_exhaustion_wins_when_never_satisfied() {
    let (vc, pc, ec) = counters();
    let verifier = Arc::new(ScriptedVerifier {
        verdicts: vec![verdict(50, false, &[])],
        calls: vc.clone(),
    });
    let planner = Arc::new(ScriptedPlanner {
        dirs: vec![
            ("d1".into(), false),
            ("d2".into(), false),
            ("d3".into(), false),
        ],
        calls: pc.clone(),
    });
    let config = LoopConfig {
        budget: 3,
        max_no_progress: 0,
        ..LoopConfig::default()
    };
    let outcome = drive_loop(
        &CountingExecutor { calls: ec.clone() },
        verifier,
        planner,
        "t".to_string(),
        config,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        outcome,
        RunOutcome::Capped {
            iterations: 3,
            which: "max_iterations",
        }
    );
}

#[tokio::test]
async fn exact_budget_mode_runs_all_iterations() {
    let (vc, pc, ec) = counters();
    let verifier = Arc::new(ScriptedVerifier {
        verdicts: vec![verdict(95, true, &[])],
        calls: vc.clone(),
    });
    let planner = Arc::new(ScriptedPlanner {
        dirs: vec![
            ("d1".into(), false),
            ("d2".into(), false),
            ("d3".into(), false),
        ],
        calls: pc.clone(),
    });
    let config = LoopConfig {
        budget: 3,
        stop_when_satisfied: false,
        max_no_progress: 0,
        ..LoopConfig::default()
    };
    let outcome = drive_loop(
        &CountingExecutor { calls: ec.clone() },
        verifier,
        planner,
        "t".to_string(),
        config,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, RunOutcome::Capped { iterations: 3, .. }));
    assert_eq!(ec.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn no_progress_detection_stops_early() {
    let (vc, pc, ec) = counters();
    let verifier = Arc::new(ScriptedVerifier {
        verdicts: vec![verdict(50, false, &["x"])],
        calls: vc.clone(),
    });
    let planner = Arc::new(ScriptedPlanner {
        dirs: vec![("d1".into(), false), ("d2".into(), false)],
        calls: pc.clone(),
    });
    let outcome = drive_loop(
        &CountingExecutor { calls: ec.clone() },
        verifier,
        planner,
        "t".to_string(),
        LoopConfig::default(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    match outcome {
        RunOutcome::Achieved { iterations, reason } => {
            assert_eq!(iterations, 3);
            assert!(reason.contains("no progress"), "reason was {reason}");
        }
        other => panic!("expected no-progress stop, got {other:?}"),
    }
}

#[tokio::test]
async fn repeated_directive_without_strategy_change_stops() {
    let (vc, pc, ec) = counters();
    let verifier = Arc::new(ScriptedVerifier {
        verdicts: vec![verdict(50, false, &["g"])],
        calls: vc.clone(),
    });
    let planner = Arc::new(ScriptedPlanner {
        dirs: vec![("same".into(), false)],
        calls: pc.clone(),
    });
    let config = LoopConfig {
        budget: 10,
        max_no_progress: 0,
        ..LoopConfig::default()
    };
    let outcome = drive_loop(
        &CountingExecutor { calls: ec.clone() },
        verifier,
        planner,
        "t".to_string(),
        config,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    match outcome {
        RunOutcome::Achieved { iterations, reason } => {
            assert_eq!(iterations, 2);
            assert!(reason.contains("repeated directive"), "reason was {reason}");
        }
        other => panic!("expected repeated-directive stop, got {other:?}"),
    }
}

#[test]
fn cost_preflight_enforces_ceiling() {
    assert!(
        cost_preflight(&LoopConfig {
            budget: 101,
            ..LoopConfig::default()
        })
        .is_err()
    );
    assert!(
        cost_preflight(&LoopConfig {
            budget: 0,
            ..LoopConfig::default()
        })
        .is_err()
    );
    assert!(
        cost_preflight(&LoopConfig {
            budget: 10,
            ..LoopConfig::default()
        })
        .unwrap()
            > 0
    );
}
