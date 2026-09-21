//! Loop mode: independent verifier + planner over iterative lead turns.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use hya_proto::{Event, Message, MessageId, ModelRef, Part, PartId, SessionId};
use hya_provider::{CompletionRequest, ProviderRouter};
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::completion::{
    GateOutcome, IterationDriver, IterationExecutor, IterationGate, RunOutcome, SafetyCaps,
    render_transcript,
};
use crate::engine::{AgentSpec, CreateSession, SessionEngine};
use crate::error::CoreError;
use crate::hooks::HookDispatcher;

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
    /// Build a predicate with the default 30s evaluation timeout: a condition
    /// that hangs is a broken condition, never a false answer.
    #[must_use]
    pub fn new(
        command: impl Into<String>,
        mode: PredicateMode,
        workdir: std::path::PathBuf,
    ) -> Self {
        Self {
            command: command.into(),
            mode,
            timeout: Duration::from_secs(30),
            workdir,
        }
    }

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

/// Clamp a requested loop budget into the range [`cost_preflight`] enforces
/// (1..=[`HARD_MAX_ITERATIONS`]). Entry points may clamp CLI/config requests
/// up front; [`cost_preflight`] remains the authoritative engine-side gate.
#[must_use]
pub fn clamp_budget(requested: u32) -> u32 {
    requested.clamp(1, HARD_MAX_ITERATIONS)
}

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
    /// Optional engine-supplied `loop.should_stop` consult, judged after the
    /// deterministic predicate and before the verifier.
    should_stop: Option<Arc<dyn HookDispatcher>>,
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
            should_stop: None,
            state: Mutex::new(LoopState::default()),
            broken_condition: std::sync::Mutex::new(None),
        }
    }

    /// Wire an optional `loop.should_stop` consult (e.g. the plugin host's
    /// dispatcher). A `Some(reason)` reply stops the loop with a
    /// `loop.should_stop:`-prefixed reason; errors are the dispatcher's to
    /// fail open.
    #[must_use]
    pub fn with_should_stop_hook(mut self, hook: Option<Arc<dyn HookDispatcher>>) -> Self {
        self.should_stop = hook;
        self
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
        // Engine-supplied post-turn consult: a `Some(reason)` is a legitimate
        // stop, decided before the verifier is even consulted. Fail-open is
        // the dispatcher's contract (errors read as `None`).
        if let Some(hook) = &self.should_stop
            && let Some(reason) = hook.loop_should_stop(&self.target, transcript).await
        {
            return Ok(GateOutcome::Stop {
                reason: format!("loop.should_stop: {reason}"),
            });
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
///
/// `should_stop` is the optional engine-supplied `loop.should_stop` consult
/// (e.g. a plugin-host dispatcher); see [`LoopGate::with_should_stop_hook`].
pub async fn drive_loop(
    executor: &dyn IterationExecutor,
    verifier: Arc<dyn LoopVerifier>,
    planner: Arc<dyn LoopPlanner>,
    target: String,
    config: LoopConfig,
    cancel: CancellationToken,
    should_stop: Option<Arc<dyn HookDispatcher>>,
) -> Result<RunOutcome, CoreError> {
    let gate = LoopGate::new(target.clone(), verifier, planner, config.clone())
        .with_should_stop_hook(should_stop);
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
///
/// `should_stop` is the optional `loop.should_stop` consult dispatched after
/// each iteration (before the verifier); see [`LoopGate::with_should_stop_hook`].
pub async fn run_loop(
    engine: Arc<SessionEngine>,
    lead_session: SessionId,
    agent: AgentSpec,
    target: String,
    verifier: Arc<dyn LoopVerifier>,
    planner: Arc<dyn LoopPlanner>,
    config: LoopConfig,
    cancel: CancellationToken,
    should_stop: Option<Arc<dyn HookDispatcher>>,
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
    drive_loop(
        &executor,
        verifier,
        planner,
        target,
        config,
        cancel,
        should_stop,
    )
    .await
}

/// System prompt for [`ModelLoopVerifier`]: an independent grader with no
/// stake in the work and no tools.
const LOOP_VERIFIER_SYSTEM: &str = "You are an independent loop verifier. You have no stake \
     in the work and no tools. Grade only the target against the transcript you are given.";

/// System prompt for [`ModelLoopPlanner`].
const LOOP_PLANNER_SYSTEM: &str =
    "You are an independent loop planner. No tools. Plan only from the inputs you are given.";

/// Neutral directive used when the planner's reply is unusable: the loop must
/// keep making progress instead of aborting or repeating a broken plan.
const NEUTRAL_LOOP_DIRECTIVE: &str = "Continue working toward the target. Re-check what the \
     verifier flagged as missing, keep changes small and verifiable, and summarize evidence.";

/// Tolerant strict-JSON object extraction (mirrors `parse_verdict` in
/// `workflow/run.rs`): pulls the outermost `{...}` span out of a reply that
/// may be fenced or prose-wrapped.
fn extract_json_object(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (end >= start).then_some(&trimmed[start..=end])
}

/// Map a wire `evidence_quality` string onto [`EvidenceQuality`].
fn evidence_quality_from_wire(value: Option<&str>) -> EvidenceQuality {
    match value {
        Some("verified") => EvidenceQuality::Verified,
        Some("supported") => EvidenceQuality::Supported,
        Some("claim_only") => EvidenceQuality::ClaimOnly,
        _ => EvidenceQuality::Missing,
    }
}

/// Production verifier: a separate model call with NO tools that grades the
/// target against the transcript and replies with ONLY
/// `{"score": 0-100, "satisfied": bool, "evidence_quality": "missing"|
/// "claim_only"|"supported"|"verified", "critical_gaps": [...],
/// "iteration_summary": "...", "reason": "..."}`.
///
/// Tolerance contract (mirrors [`crate::completion::ModelGoalEvaluator`]): a
/// malformed reply, or a failing provider chain, degrades to a not-satisfied
/// verdict with score 0 and [`EvidenceQuality::Missing`] — a broken verdict
/// counts against the loop's no-progress/cap machinery instead of aborting
/// or satisfying the run.
pub struct ModelLoopVerifier {
    providers: Arc<ProviderRouter>,
    model: ModelRef,
}

impl ModelLoopVerifier {
    /// Build a verifier that routes to `model` through `providers`.
    #[must_use]
    pub fn new(providers: Arc<ProviderRouter>, model: ModelRef) -> Self {
        Self { providers, model }
    }
}

#[derive(Deserialize)]
struct VerifierReplyJson {
    #[serde(default)]
    score: Option<u8>,
    #[serde(default)]
    satisfied: Option<bool>,
    #[serde(default)]
    evidence_quality: Option<String>,
    #[serde(default)]
    critical_gaps: Option<Vec<String>>,
    #[serde(default)]
    iteration_summary: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

/// The not-satisfied verdict every tolerance path degrades to.
fn not_satisfied(reason: String) -> VerifierVerdict {
    VerifierVerdict {
        score: 0,
        satisfied: false,
        evidence_quality: EvidenceQuality::Missing,
        critical_gaps: Vec::new(),
        iteration_summary: String::new(),
        reason,
    }
}

/// Reason recorded when the verifier's reply is not a parseable verdict.
const VERIFIER_MALFORMED_REASON: &str = "verifier returned malformed output";

#[async_trait]
impl LoopVerifier for ModelLoopVerifier {
    async fn grade(&self, target: &str, transcript: &str) -> Result<VerifierVerdict, CoreError> {
        let prompt = format!(
            "## TARGET\n{target}\n\n## TRANSCRIPT\n{transcript}\n\nReply with ONLY a JSON \
             object: {{\"score\": 0-100, \"satisfied\": true|false, \"evidence_quality\": \
             \"missing\"|\"claim_only\"|\"supported\"|\"verified\", \"critical_gaps\": [\"...\"], \
             \"iteration_summary\": \"...\", \"reason\": \"...\"}}. Judge only from the \
             transcript; if you cannot see evidence the target is met, answer satisfied=false."
        );
        let request = CompletionRequest {
            model: self.model.clone(),
            system: Some(LOOP_VERIFIER_SYSTEM.to_string()),
            messages: vec![Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: prompt,
                }],
            }],
            tools: Vec::new(),
            temperature: Some(0.0),
            max_output_tokens: Some(1024),
            reasoning: None,
            headers: Default::default(),
        };
        let mut stream = match self
            .providers
            .stream(request, SessionId::new(), MessageId::new())
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                return Ok(not_satisfied(format!("verifier provider failed: {error}")));
            }
        };
        let mut text = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(Event::TextDelta { delta, .. }) => text.push_str(&delta),
                // A broken provider chain grades not-satisfied: the loop stays
                // cappable instead of erroring the run.
                Err(error) => {
                    return Ok(not_satisfied(format!("verifier provider failed: {error}")));
                }
                Ok(_) => {}
            }
        }
        let Some(json) = extract_json_object(&text) else {
            return Ok(not_satisfied(VERIFIER_MALFORMED_REASON.to_string()));
        };
        match serde_json::from_str::<VerifierReplyJson>(json) {
            Ok(reply) => Ok(VerifierVerdict {
                score: reply.score.unwrap_or(0),
                satisfied: reply.satisfied.unwrap_or(false),
                evidence_quality: evidence_quality_from_wire(reply.evidence_quality.as_deref()),
                critical_gaps: reply.critical_gaps.unwrap_or_default(),
                iteration_summary: reply.iteration_summary.unwrap_or_default(),
                reason: reply
                    .reason
                    .unwrap_or_else(|| "no reason given".to_string()),
            }),
            Err(_) => Ok(not_satisfied(VERIFIER_MALFORMED_REASON.to_string())),
        }
    }
}

/// Production planner: a separate model call with NO tools that plans the
/// next directive from history and the last verdict, replying with ONLY
/// `{"directive": "...", "continuity_brief": "...", "planner_notes": "...",
/// "strategy_change": bool}`.
///
/// Tolerance contract: a malformed reply or a failing provider chain degrades
/// to a neutral continue directive, so the loop keeps making progress and
/// stays cappable instead of erroring the run.
pub struct ModelLoopPlanner {
    providers: Arc<ProviderRouter>,
    model: ModelRef,
}

impl ModelLoopPlanner {
    /// Build a planner that routes to `model` through `providers`.
    #[must_use]
    pub fn new(providers: Arc<ProviderRouter>, model: ModelRef) -> Self {
        Self { providers, model }
    }
}

#[derive(Deserialize)]
struct PlannerReplyJson {
    #[serde(default)]
    directive: Option<String>,
    #[serde(default)]
    continuity_brief: Option<String>,
    #[serde(default)]
    planner_notes: Option<String>,
    #[serde(default)]
    strategy_change: Option<bool>,
    #[serde(default)]
    change_note: Option<String>,
}

#[async_trait]
impl LoopPlanner for ModelLoopPlanner {
    async fn plan_next(
        &self,
        target: &str,
        history: &[String],
        last: &VerifierVerdict,
        planner_notes: &str,
    ) -> Result<PlannerOutput, CoreError> {
        let prompt = format!(
            "## TARGET\n{target}\n\n## ITERATION SUMMARIES\n{}\n\n## LAST VERDICT\nscore={} \
             satisfied={} evidence_quality={:?} gaps={:?} reason={}\n\n## PLANNER NOTES\n\
             {planner_notes}\n\nReply with ONLY a JSON object: {{\"directive\": \"...\", \
             \"continuity_brief\": \"...\", \"planner_notes\": \"...\", \"strategy_change\": \
             true|false}}. Plan the single next step that closes the largest gap.",
            history.join("\n"),
            last.score,
            last.satisfied,
            last.evidence_quality,
            last.critical_gaps,
            last.reason,
        );
        let request = CompletionRequest {
            model: self.model.clone(),
            system: Some(LOOP_PLANNER_SYSTEM.to_string()),
            messages: vec![Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: prompt,
                }],
            }],
            tools: Vec::new(),
            temperature: Some(0.0),
            max_output_tokens: Some(1024),
            reasoning: None,
            headers: Default::default(),
        };
        let streamed = async {
            let mut stream = self
                .providers
                .stream(request, SessionId::new(), MessageId::new())
                .await?;
            let mut text = String::new();
            while let Some(item) = stream.next().await {
                match item {
                    Ok(Event::TextDelta { delta, .. }) => text.push_str(&delta),
                    // A broken provider chain plans neutrally: the loop stays
                    // cappable instead of erroring the run.
                    Err(error) => return Ok::<String, CoreError>(error.to_string()),
                    Ok(_) => {}
                }
            }
            Ok(text)
        }
        .await;
        let text = match streamed {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(%error, "loop planner provider failed; planning neutrally");
                return Ok(neutral_plan());
            }
        };
        let Some(json) = extract_json_object(&text) else {
            return Ok(neutral_plan());
        };
        match serde_json::from_str::<PlannerReplyJson>(json) {
            Ok(reply) => {
                let Some(directive) = reply.directive.filter(|d| !d.trim().is_empty()) else {
                    return Ok(neutral_plan());
                };
                Ok(PlannerOutput {
                    directive,
                    continuity_brief: reply.continuity_brief.unwrap_or_default(),
                    planner_notes: reply.planner_notes.unwrap_or_default(),
                    strategy_change: reply.strategy_change.unwrap_or(false),
                    change_note: reply.change_note.unwrap_or_default(),
                })
            }
            Err(_) => Ok(neutral_plan()),
        }
    }
}

/// The neutral continue plan every planner tolerance path degrades to.
fn neutral_plan() -> PlannerOutput {
    PlannerOutput {
        directive: NEUTRAL_LOOP_DIRECTIVE.to_string(),
        continuity_brief: String::new(),
        planner_notes: String::new(),
        strategy_change: false,
        change_note: String::new(),
    }
}
