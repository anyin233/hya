//! Loop mode: independent verifier + planner over iterative lead turns.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::SessionId;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::completion::{
    GateOutcome, IterationDriver, IterationExecutor, IterationGate, RunOutcome, SafetyCaps,
    render_transcript,
};
use crate::engine::{AgentSpec, CreateSession, SessionEngine};
use crate::error::CoreError;

/// How strongly the transcript supports the loop target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EvidenceQuality {
    /// No relevant evidence.
    Missing,
    /// Claims without support.
    ClaimOnly,
    /// Partial supporting evidence.
    Supported,
    /// Fully verified against the target.
    Verified,
}

/// Result of one verifier pass.
#[derive(Clone, Debug)]
pub struct VerifierVerdict {
    /// Score 0–100 used with [`LoopConfig::satisfaction_threshold`].
    pub score: u8,
    /// Whether the verifier considers the target satisfied.
    pub satisfied: bool,
    /// Evidence quality band.
    pub evidence_quality: EvidenceQuality,
    /// Remaining gaps the planner should address.
    pub critical_gaps: Vec<String>,
    /// Short summary of this iteration's work.
    pub iteration_summary: String,
    /// Free-form reason string.
    pub reason: String,
}

/// Independent loop grader (not the worker agent).
///
/// **Contract:** Grade only `target` + `transcript`. Do not mutate the session.
#[async_trait]
pub trait LoopVerifier: Send + Sync {
    /// Produce a structured verdict for the current transcript.
    ///
    /// # Errors
    /// Propagate model/runtime failures.
    async fn grade(&self, target: &str, transcript: &str) -> Result<VerifierVerdict, CoreError>;
}

/// Planner output for the next worker directive.
#[derive(Clone, Debug)]
pub struct PlannerOutput {
    /// Next directive for the lead turn.
    pub directive: String,
    /// Continuity notes for the worker.
    pub continuity_brief: String,
    /// Notes retained across iterations.
    pub planner_notes: String,
    /// Whether strategy changed this step.
    pub strategy_change: bool,
    /// Explanation of strategy change.
    pub change_note: String,
}

/// Produces the next worker directive from history and the last verdict.
///
/// **Contract:** Pure planning relative to inputs; engine owns stop decisions via
/// the verifier and [`LoopConfig`].
#[async_trait]
pub trait LoopPlanner: Send + Sync {
    /// Plan the next directive.
    ///
    /// # Errors
    /// Propagate model/runtime failures.
    async fn plan_next(
        &self,
        target: &str,
        history: &[String],
        last: &VerifierVerdict,
        planner_notes: &str,
    ) -> Result<PlannerOutput, CoreError>;
}

/// Tunables for loop satisfaction and no-progress detection.
#[derive(Clone, Debug)]
pub struct LoopConfig {
    /// Maximum iterations (also bounded by hard ceiling in preflight).
    pub budget: u32,
    /// Stop when verifier marks satisfied above threshold.
    pub stop_when_satisfied: bool,
    /// Minimum score treated as satisfied when `stop_when_satisfied`.
    pub satisfaction_threshold: u8,
    /// Consecutive no-progress iterations before giving up.
    pub max_no_progress: u32,
    /// Deterministic exit predicate. When present it outranks the model
    /// verifier's satisfied verdict: the predicate alone decides stop-vs-
    /// continue, and the verifier only feeds the planner.
    pub predicate: Option<LoopPredicate>,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            budget: 10,
            stop_when_satisfied: true,
            satisfaction_threshold: 90,
            max_no_progress: 3,
            predicate: None,
        }
    }
}

/// Which direction a [`LoopPredicate`] reads in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PredicateMode {
    /// Exit 0 continues the loop; exit 1 stops it.
    While,
    /// Exit 0 stops the loop; exit 1 continues it.
    Until,
}

impl PredicateMode {
    /// Wire/config spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::While => "while",
            Self::Until => "until",
        }
    }
}

/// The result of one deterministic predicate evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoopPredicateOutcome {
    /// The stop condition is met (exit code matched the mode's stop answer).
    Satisfied {
        /// Human-readable stop reason for the run outcome.
        reason: String,
    },
    /// Keep iterating.
    Continue,
    /// The condition itself is broken (non-answer exit code or timeout).
    /// Stopping here is a failure, never a success: a typo'd condition must
    /// not look like finished work.
    Broken {
        /// What made the condition unusable.
        detail: String,
    },
}

/// Deterministic loop-exit predicate: `--while '<cmd>'` / `--until '<cmd>'`.
///
/// The exit code is the only signal — stdout is ignored. Exit 0/1 are the
/// condition's answer (inverted between [`PredicateMode::While`] and
/// [`PredicateMode::Until`]); any other exit code or a timeout means the
/// condition itself is broken, which stops the loop as a failure rather than
/// silently looking like finished work.
#[derive(Clone, Debug)]
pub struct LoopPredicate {
    /// Shell command text, run as `sh -c <command>` in `workdir`.
    pub command: String,
    /// Answer direction.
    pub mode: PredicateMode,
    /// Evaluation timeout; a timed-out condition is broken, not false.
    pub timeout: Duration,
    /// Working directory for the command.
    pub workdir: std::path::PathBuf,
}

impl LoopPredicate {
    /// Evaluate the condition once, synchronously, with the configured
    /// timeout. Blocking by design: loop gates run between iterations.
    #[must_use]
    pub fn evaluate(&self) -> LoopPredicateOutcome {
        let Ok(mut child) = std::process::Command::new("sh")
            .arg("-c")
            .arg(&self.command)
            .current_dir(&self.workdir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return LoopPredicateOutcome::Broken {
                detail: format!("failed to spawn condition command `{}`", self.command),
            };
        };
        let deadline = std::time::Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => {
                    let _ = child.kill();
                    return LoopPredicateOutcome::Broken {
                        detail: format!("condition wait failed: {error}"),
                    };
                }
            }
        };
        let Some(status) = status else {
            return LoopPredicateOutcome::Broken {
                detail: format!(
                    "condition `{}` timed out after {:?}",
                    self.command, self.timeout
                ),
            };
        };
        let Some(code) = status.code() else {
            return LoopPredicateOutcome::Broken {
                detail: format!(
                    "condition `{}` terminated by signal; the condition itself is broken",
                    self.command
                ),
            };
        };
        match (self.mode, code) {
            (PredicateMode::Until, 0) | (PredicateMode::While, 1) => {
                LoopPredicateOutcome::Satisfied {
                    reason: format!(
                        "{} `{}` answered with exit {code}",
                        self.mode.as_str(),
                        self.command
                    ),
                }
            }
            (PredicateMode::Until, 1) | (PredicateMode::While, 0) => LoopPredicateOutcome::Continue,
            _ => LoopPredicateOutcome::Broken {
                detail: format!(
                    "condition `{}` exited {code}; the condition itself is broken",
                    self.command
                ),
            },
        }
    }
}

const HARD_MAX_ITERATIONS: u32 = 100;

/// Reject an unservable loop before any worker runs (engine authority): an
/// explicit budget within the hard ceiling. Returns a worst-case token estimate.
pub fn cost_preflight(config: &LoopConfig) -> Result<u64, CoreError> {
    if config.budget == 0 || config.budget > HARD_MAX_ITERATIONS {
        return Err(CoreError::Invalid(format!(
            "loop budget must be 1..={HARD_MAX_ITERATIONS}, got {}",
            config.budget
        )));
    }
    let per_iteration = 500_000u64 + 256 + 2_000;
    Ok(u64::from(config.budget) * per_iteration)
}

fn fingerprint(items: &[String]) -> u64 {
    let mut h = DefaultHasher::new();
    items.hash(&mut h);
    h.finish()
}

#[derive(Default)]
struct LoopState {
    history: Vec<String>,
    planner_notes: String,
    recent_directive_fps: Vec<u64>,
    last_gap_fp: Option<u64>,
    no_progress: u32,
}

/// [`IterationGate`] combining verifier + planner with no-progress tracking.
pub struct LoopGate {
    target: String,
    verifier: Arc<dyn LoopVerifier>,
    planner: Arc<dyn LoopPlanner>,
    config: LoopConfig,
    state: Mutex<LoopState>,
    broken_condition: std::sync::Mutex<Option<String>>,
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
            state: Mutex::new(LoopState::default()),
            broken_condition: std::sync::Mutex::new(None),
        }
    }

    /// The broken-condition detail when the last judgment stopped because the
    /// deterministic predicate itself failed (bad exit code or timeout).
    /// Entry points must surface this as a failure, never as success.
    #[must_use]
    pub fn broken_condition(&self) -> Option<String> {
        self.broken_condition
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl IterationGate for LoopGate {
    async fn judge(&self, transcript: &str) -> Result<GateOutcome, CoreError> {
        // The deterministic predicate outranks the model verdict: it alone
        // decides stop-vs-continue, and a broken condition stops as a failure.
        if let Some(predicate) = &self.config.predicate {
            match predicate.evaluate() {
                LoopPredicateOutcome::Satisfied { reason } => {
                    return Ok(GateOutcome::Stop {
                        reason: format!(
                            "{} condition {reason}",
                            self.config
                                .predicate
                                .as_ref()
                                .map_or(String::new(), |_| String::new())
                        ),
                    });
                }
                LoopPredicateOutcome::Broken { detail } => {
                    *self
                        .broken_condition
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detail.clone());
                    return Ok(GateOutcome::Stop {
                        reason: format!("broken condition: {detail}"),
                    });
                }
                LoopPredicateOutcome::Continue => {}
            }
        }
        let mut st = self.state.lock().await;
        let verdict = self.verifier.grade(&self.target, transcript).await?;
        st.history.push(verdict.iteration_summary.clone());

        // Engine authority: only the verifier (not the planner) can declare success.
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
                return Ok(GateOutcome::Stop {
                    reason: format!("no progress for {} iterations", self.config.max_no_progress),
                });
            }
        }

        let notes = st.planner_notes.clone();
        let plan = self
            .planner
            .plan_next(&self.target, &st.history, &verdict, &notes)
            .await?;

        let directive_fp = fingerprint(std::slice::from_ref(&plan.directive));
        if st.recent_directive_fps.contains(&directive_fp) && !plan.strategy_change {
            return Ok(GateOutcome::Stop {
                reason: "repeated directive without strategy change".to_string(),
            });
        }
        st.recent_directive_fps.push(directive_fp);
        if st.recent_directive_fps.len() > 2 {
            st.recent_directive_fps.remove(0);
        }
        st.planner_notes = plan.planner_notes;

        Ok(GateOutcome::Continue {
            directive: format!("{}\n\n{}", plan.directive, plan.continuity_brief),
        })
    }
}

/// [`IterationExecutor`] that runs each loop step in a fresh child session.
pub struct WorkerSessionExecutor {
    engine: Arc<SessionEngine>,
    lead_session: SessionId,
    agent: AgentSpec,
    binding: crate::TurnBinding,
}

#[async_trait]
impl IterationExecutor for WorkerSessionExecutor {
    async fn run_iteration(
        &self,
        directive: &str,
        cancel: &CancellationToken,
    ) -> Result<String, CoreError> {
        let child = self
            .engine
            .create(CreateSession {
                parent: Some(self.lead_session),
                agent: self.agent.name.clone(),
                model: self.agent.model.clone(),
                workdir: self.agent.workdir.to_string_lossy().into_owned(),
            })
            .await?;
        self.engine
            .admit_user_prompt(child, directive.to_string())
            .await?;
        self.engine
            .run_bound_turn(
                child,
                &self.agent,
                self.binding.clone(),
                cancel.clone(),
                None,
            )
            .await?;
        let projection = self.engine.read_projection(child).await?;
        Ok(render_transcript(&projection))
    }
}

/// Drive loop iterations with the given gate/executor under safety caps.
pub async fn drive_loop(
    executor: &dyn IterationExecutor,
    verifier: Arc<dyn LoopVerifier>,
    planner: Arc<dyn LoopPlanner>,
    target: String,
    config: LoopConfig,
    cancel: CancellationToken,
) -> Result<RunOutcome, CoreError> {
    let gate = LoopGate::new(target.clone(), verifier, planner, config.clone());
    let caps = SafetyCaps {
        max_iterations: config.budget,
        ..SafetyCaps::default()
    };
    IterationDriver::new(caps)
        .run(executor, &gate, target, cancel)
        .await
}

#[allow(clippy::too_many_arguments)]
/// High-level loop mode entry: create gate and drive until outcome.
pub async fn run_loop(
    engine: Arc<SessionEngine>,
    lead_session: SessionId,
    agent: AgentSpec,
    target: String,
    verifier: Arc<dyn LoopVerifier>,
    planner: Arc<dyn LoopPlanner>,
    config: LoopConfig,
    cancel: CancellationToken,
) -> Result<RunOutcome, CoreError> {
    cost_preflight(&config)?;
    let binding = engine
        .bind_session_runtime(lead_session, &agent.workdir)
        .await?;
    let executor = WorkerSessionExecutor {
        engine,
        lead_session,
        agent,
        binding,
    };
    drive_loop(&executor, verifier, planner, target, config, cancel).await
}
