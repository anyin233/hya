//! [`LoopGate`] combining deterministic predicate, `loop.should_stop` consult,
//! verifier grading, no-progress detection, and planner directives.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use async_trait::async_trait;
use hya_runtime_abi::HostError;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::predicate::LoopPredicateOutcome;
use crate::types::{
    EvidenceQuality, GateOutcome, IterationGate, LoopConfig, LoopPlanner, LoopShouldStopHook,
    LoopVerifier,
};

fn fingerprint(items: &[String]) -> u64 {
    let mut hasher = DefaultHasher::new();
    items.hash(&mut hasher);
    hasher.finish()
}

#[derive(Default)]
struct LoopState {
    history: Vec<String>,
    planner_notes: String,
    recent_directive_fps: Vec<u64>,
    last_gap_fp: Option<u64>,
    no_progress: u32,
}

/// [`IterationGate`] combining deterministic predicate, `loop.should_stop`
/// consult, independent verifier, and planner with no-progress tracking.
pub struct LoopGate {
    target: String,
    verifier: Arc<dyn LoopVerifier>,
    planner: Arc<dyn LoopPlanner>,
    config: LoopConfig,
    should_stop: Option<Arc<dyn LoopShouldStopHook>>,
    state: Mutex<LoopState>,
}

impl LoopGate {
    /// Build a gate for `target` with the given verifier, planner, and config.
    #[must_use]
    pub fn new(
        target: String,
        verifier: Arc<dyn LoopVerifier>,
        planner: Arc<dyn LoopPlanner>,
        config: LoopConfig,
    ) -> Self {
        Self {
            target,
            verifier,
            planner,
            config,
            should_stop: None,
            state: Mutex::new(LoopState::default()),
        }
    }

    /// Wire an optional `loop.should_stop` consult. A `Some(reason)` reply stops
    /// the loop with a `loop.should_stop:`-prefixed reason; errors are fail-open.
    #[must_use]
    pub fn with_should_stop_hook(mut self, hook: Option<Arc<dyn LoopShouldStopHook>>) -> Self {
        self.should_stop = hook;
        self
    }
}

#[async_trait]
impl IterationGate for LoopGate {
    async fn judge(
        &self,
        transcript: &str,
        cancel: &CancellationToken,
    ) -> Result<GateOutcome, HostError> {
        let judgment = async {
            // The deterministic predicate outranks the model verdict: it alone
            // decides stop-vs-continue, and a broken condition stops as a failure.
            if let Some(predicate) = &self.config.predicate {
                match predicate.evaluate().await {
                    LoopPredicateOutcome::Satisfied { reason } => {
                        return Ok(GateOutcome::Stop {
                            reason: format!("condition {reason}"),
                        });
                    }
                    LoopPredicateOutcome::Broken { detail } => {
                        return Ok(GateOutcome::InvalidCondition {
                            reason: format!("broken condition: {detail}"),
                        });
                    }
                    LoopPredicateOutcome::Continue => {}
                }
            }
            // Host-supplied post-turn consult: a `Some(reason)` is a legitimate
            // stop, decided before the verifier is even consulted. Fail-open is
            // the hook's contract (errors read as `None`).
            if let Some(hook) = &self.should_stop
                && let Some(reason) = hook.should_stop(&self.target, transcript).await
            {
                return Ok(GateOutcome::Stop {
                    reason: format!("loop.should_stop: {reason}"),
                });
            }
            let verdict = self.verifier.grade(&self.target, transcript).await?;
            let mut st = self.state.lock().await;
            st.history.push(verdict.iteration_summary.clone());

            // Driver authority: only the verifier (not the planner) can declare success.
            if self.config.predicate.is_none()
                && self.config.stop_when_satisfied
                && verdict.satisfied
                && verdict.score >= self.config.satisfaction_threshold
                && verdict.critical_gaps.is_empty()
                && verdict.evidence_quality >= EvidenceQuality::Supported
            {
                return Ok(GateOutcome::Stop {
                    reason: format!("satisfied: score {}", verdict.score),
                });
            }

            let gap_fp = fingerprint(&verdict.critical_gaps);
            if self.config.max_no_progress > 0 {
                if st.last_gap_fp == Some(gap_fp) {
                    st.no_progress += 1;
                } else {
                    st.no_progress = 1;
                    st.last_gap_fp = Some(gap_fp);
                }
                if st.no_progress >= self.config.max_no_progress {
                    return Ok(GateOutcome::Stalled);
                }
            }

            let notes = st.planner_notes.clone();
            let history = st.history.clone();
            drop(st);
            let plan = self
                .planner
                .plan_next(&self.target, &history, &verdict, &notes)
                .await?;

            let mut st = self.state.lock().await;
            let directive_fp = fingerprint(std::slice::from_ref(&plan.directive));
            if st.recent_directive_fps.contains(&directive_fp) && !plan.strategy_change {
                return Ok(GateOutcome::Stalled);
            }
            st.recent_directive_fps.push(directive_fp);
            if st.recent_directive_fps.len() > 2 {
                st.recent_directive_fps.remove(0);
            }
            st.planner_notes = plan.planner_notes;

            Ok(GateOutcome::Continue {
                directive: format!("{}\n\n{}", plan.directive, plan.continuity_brief),
            })
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(HostError::Cancelled),
            result = judgment => result,
        }
    }
}
