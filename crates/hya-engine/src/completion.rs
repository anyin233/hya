//! Shared iteration primitives retained for transitional loop-mode and
//! workflow stage drivers (goal-mode driver and evaluators now live in
//! `bundles/builtin/goal`).

use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hya_proto::{PartProjection, Projection};
use tokio_util::sync::CancellationToken;

use crate::error::CoreError;

/// Hard stops for iteration drivers.
#[derive(Clone, Copy, Debug)]
pub struct SafetyCaps {
    /// Maximum worker iterations before `Capped`.
    pub max_iterations: u32,
    /// Wall-clock budget for the whole run.
    pub max_wall_clock: Duration,
}

impl Default for SafetyCaps {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            max_wall_clock: Duration::from_secs(1800),
        }
    }
}

/// Terminal status of an iterative run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// Gate reported stop (target satisfied).
    Achieved {
        /// Iterations completed including the final one.
        iterations: u32,
        /// Gate reason string.
        reason: String,
    },
    /// Hit a safety cap before success.
    Capped {
        /// Iterations completed.
        iterations: u32,
        /// Which cap fired (`max_iterations`, `max_wall_clock`, …).
        which: &'static str,
    },
    /// Hit a safety cap, ran one wrap-up iteration, and stopped. Budget
    /// exhaustion is never forged into [`RunOutcome::Achieved`].
    BudgetLimited {
        /// Iterations completed before the wrap-up pass.
        iterations: u32,
        /// Which cap fired (`max_iterations`, `max_wall_clock`, …).
        which: &'static str,
    },
    /// Progress stopped: repeated activity, unchanged gaps, or a repeated
    /// directive ended the run before exhausting the remaining budget.
    Stalled {
        /// Iterations completed including the repeated one.
        iterations: u32,
    },
    /// The deterministic loop condition could not be evaluated.
    InvalidCondition {
        /// Completed worker iterations.
        iterations: u32,
        /// Condition failure description.
        reason: String,
    },
    /// Cancellation token tripped.
    Cancelled,
}

/// Decision from an [`IterationGate`] after one transcript.
pub enum GateOutcome {
    /// Stop the driver successfully.
    Stop {
        /// Reason recorded on [`RunOutcome::Achieved`].
        reason: String,
    },
    /// Stop without achieving the target.
    Stalled,
    /// Stop because the deterministic condition is unusable.
    InvalidCondition {
        /// Condition failure description.
        reason: String,
    },
    /// Run another worker iteration with this directive.
    Continue {
        /// Next prompt/directive for the executor.
        directive: String,
    },
}

/// Independent stop authority for iterative runs.
///
/// **Contract:** Called after each executor iteration with the rendered
/// transcript. Must honor cancellation and finish any owned cleanup before
/// returning. Errors abort the whole run.
#[async_trait]
pub trait IterationGate: Send + Sync {
    /// Judge whether to stop or continue.
    ///
    /// # Errors
    /// Return [`CoreError`] to fail the run.
    async fn judge(
        &self,
        transcript: &str,
        cancel: &CancellationToken,
    ) -> Result<GateOutcome, CoreError>;
}

/// Runs one agent iteration given a directive string.
///
/// **Contract:** Implementors admit work and return a transcript string for the
/// gate. Must honour `cancel`. Errors abort the driver.
#[async_trait]
pub trait IterationExecutor: Send + Sync {
    /// Execute one iteration and return a transcript for judging.
    ///
    /// # Errors
    /// Propagate turn/store failures.
    async fn run_iteration(
        &self,
        directive: &str,
        cancel: &CancellationToken,
    ) -> Result<String, CoreError>;
}

/// Drives executor/gate pairs under [`SafetyCaps`].
pub struct IterationDriver {
    /// Caps applied each loop.
    pub caps: SafetyCaps,
    /// Behavioral stall guard: stop (as [`RunOutcome::Stalled`]) when two
    /// consecutive iterations render identical activity.
    stall_guard: bool,
}

impl IterationDriver {
    /// Create a driver with the given caps.
    #[must_use]
    pub fn new(caps: SafetyCaps) -> Self {
        Self {
            caps,
            stall_guard: false,
        }
    }

    /// Enable the behavioral stall guard.
    #[must_use]
    pub fn with_stall_guard(mut self) -> Self {
        self.stall_guard = true;
        self
    }

    /// Run until the gate stops, a cap trips, or cancel fires.
    ///
    /// # Errors
    /// Propagates executor/gate errors.
    pub async fn run(
        &self,
        executor: &dyn IterationExecutor,
        gate: &dyn IterationGate,
        initial_directive: String,
        cancel: CancellationToken,
    ) -> Result<RunOutcome, CoreError> {
        let start = Instant::now();
        let mut directive = initial_directive;
        let mut iterations = 0u32;
        let mut previous_transcript: Option<String> = None;
        let mut previous_activity: Option<u64> = None;
        loop {
            if cancel.is_cancelled() {
                return Ok(RunOutcome::Cancelled);
            }
            let which = if iterations >= self.caps.max_iterations {
                Some("max_iterations")
            } else if start.elapsed() >= self.caps.max_wall_clock {
                Some("max_wall_clock")
            } else {
                None
            };
            if let Some(which) = which {
                if iterations == 0 {
                    return Ok(RunOutcome::Capped { iterations, which });
                }
                match executor
                    .run_iteration(&wrap_up_directive(which), &cancel)
                    .await
                {
                    Err(CoreError::Cancelled) => return Ok(RunOutcome::Cancelled),
                    result => {
                        result?;
                    }
                }
                if cancel.is_cancelled() {
                    return Ok(RunOutcome::Cancelled);
                }
                return Ok(RunOutcome::BudgetLimited { iterations, which });
            }
            iterations += 1;
            let transcript = match executor.run_iteration(&directive, &cancel).await {
                Err(CoreError::Cancelled) => return Ok(RunOutcome::Cancelled),
                result => result?,
            };
            if cancel.is_cancelled() {
                return Ok(RunOutcome::Cancelled);
            }
            let activity = previous_transcript
                .as_ref()
                .and_then(|previous| transcript.strip_prefix(previous.as_str()))
                .map(str::to_string)
                .unwrap_or_else(|| transcript.clone());
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            activity.hash(&mut hasher);
            let activity_fp = hasher.finish();
            if self.stall_guard && previous_activity == Some(activity_fp) {
                return Ok(RunOutcome::Stalled { iterations });
            }
            previous_activity = Some(activity_fp);
            previous_transcript = Some(transcript.clone());
            let judgment = match gate.judge(&transcript, &cancel).await {
                Err(CoreError::Cancelled) => return Ok(RunOutcome::Cancelled),
                result => result?,
            };
            if cancel.is_cancelled() {
                return Ok(RunOutcome::Cancelled);
            }
            match judgment {
                GateOutcome::Stop { reason } => {
                    return Ok(RunOutcome::Achieved { iterations, reason });
                }
                GateOutcome::Stalled => return Ok(RunOutcome::Stalled { iterations }),
                GateOutcome::InvalidCondition { reason } => {
                    return Ok(RunOutcome::InvalidCondition { iterations, reason });
                }
                GateOutcome::Continue { directive: next } => directive = next,
            }
        }
    }
}

/// Wrap-up directive for the [`RunOutcome::BudgetLimited`] pass: no new
/// substantive work, hand off state instead.
#[must_use]
pub fn wrap_up_directive(which: &str) -> String {
    format!(
        "BUDGET LIMIT REACHED ({which}). Do not start any new substantive work. \
         Wrap up now: report what was completed, the current verifiable state, \
         and the remaining work as a handoff. Budget exhaustion is not completion."
    )
}

/// Render a simple role-tagged transcript string from a session projection.
#[must_use]
pub fn render_transcript(projection: &Projection) -> String {
    let mut s = String::new();
    for m in &projection.session.messages {
        let mut text = String::new();
        for p in &m.parts {
            match p {
                PartProjection::Text { text: t, .. } => text.push_str(t),
                PartProjection::Tool { name, state, .. } => {
                    text.push_str(&format!("[tool:{name}:{state:?}]"));
                }
                PartProjection::Reasoning { .. } => {}
            }
        }
        s.push_str(&format!("[{:?}] {}\n", m.role, text));
    }
    s
}
