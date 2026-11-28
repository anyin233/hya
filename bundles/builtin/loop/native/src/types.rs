//! Core domain types, traits, budget validation, and transcript rendering for `hya/loop`.

use std::time::Duration;

use async_trait::async_trait;
use hya_proto::{PartProjection, Projection};
use hya_runtime_abi::{HostError, NativeVerifierVerdict};
use tokio_util::sync::CancellationToken;

use crate::predicate::LoopPredicate;

/// Canonical bundle identity of the built-in loop bundle.
pub const LOOP_BUNDLE_ID: &str = "hya/loop";

/// Stable driver identifier registered on [`hya_runtime_abi::NativeRegistrations::loop_drivers`].
pub const LOOP_DRIVER_ID: &str = "loop";

/// Stable service identifier registered on [`hya_runtime_abi::NativeRegistrations::services`].
pub const LOOP_SERVICE_ID: &str = "loop";

/// Hard upper bound on iterations enforced before any worker runs.
pub const HARD_MAX_ITERATIONS: u32 = 100;

/// Default iteration budget for loop runs.
pub const DEFAULT_LOOP_BUDGET: u32 = 10;

/// Default wall-clock safety cap in seconds (30 minutes).
pub const DEFAULT_MAX_WALL_CLOCK_SECS: u64 = 1800;

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

impl EvidenceQuality {
    /// Map a wire string onto [`EvidenceQuality`].
    #[must_use]
    pub fn from_wire(value: Option<&str>) -> Self {
        match value {
            Some("verified") => Self::Verified,
            Some("supported") => Self::Supported,
            Some("claim_only") => Self::ClaimOnly,
            _ => Self::Missing,
        }
    }

    /// Return the canonical snake-case wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::ClaimOnly => "claim_only",
            Self::Supported => "supported",
            Self::Verified => "verified",
        }
    }
}

/// Result of one verifier pass.
#[derive(Clone, Debug, PartialEq, Eq)]
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

impl VerifierVerdict {
    /// Convert from the ABI v2 [`NativeVerifierVerdict`] representation.
    #[must_use]
    pub fn from_native(native: NativeVerifierVerdict) -> Self {
        Self {
            score: native.score,
            satisfied: native.satisfied,
            evidence_quality: EvidenceQuality::from_wire(Some(native.evidence_quality.as_str())),
            critical_gaps: native.critical_gaps,
            iteration_summary: native.iteration_summary,
            reason: native.reason,
        }
    }

    /// Convert into the ABI v2 [`NativeVerifierVerdict`] representation.
    #[must_use]
    pub fn to_native(&self) -> NativeVerifierVerdict {
        NativeVerifierVerdict {
            score: self.score,
            satisfied: self.satisfied,
            evidence_quality: self.evidence_quality.as_str().to_string(),
            critical_gaps: self.critical_gaps.clone(),
            iteration_summary: self.iteration_summary.clone(),
            reason: self.reason.clone(),
        }
    }
}

/// Independent loop grader (not the worker agent).
///
/// **Contract:** Grade only `target` + `transcript`. Do not mutate the session.
#[async_trait]
pub trait LoopVerifier: Send + Sync {
    /// Produce a structured verdict for the current transcript.
    ///
    /// # Errors
    /// Propagates host/runtime failures.
    async fn grade(&self, target: &str, transcript: &str) -> Result<VerifierVerdict, HostError>;
}

/// Planner output for the next worker directive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannerOutput {
    /// Next directive for the worker turn.
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
/// **Contract:** Pure planning relative to inputs; the loop gate owns stop
/// decisions via the deterministic predicate, `loop.should_stop` consult,
/// verifier, and [`LoopConfig`].
#[async_trait]
pub trait LoopPlanner: Send + Sync {
    /// Plan the next directive.
    ///
    /// # Errors
    /// Propagates host/runtime failures.
    async fn plan_next(
        &self,
        target: &str,
        history: &[String],
        last: &VerifierVerdict,
        planner_notes: &str,
    ) -> Result<PlannerOutput, HostError>;
}

/// Consulted after each iteration (after any deterministic predicate, before
/// the verifier) to allow hooks/plugins to force a legitimate stop.
#[async_trait]
pub trait LoopShouldStopHook: Send + Sync {
    /// Return `Some(reason)` to stop the loop cleanly, or `None` to continue to
    /// the verifier.
    async fn should_stop(&self, target: &str, transcript: &str) -> Option<String>;
}

/// Tunables for loop satisfaction and no-progress detection.
#[derive(Clone, Debug)]
pub struct LoopConfig {
    /// Maximum iterations (bounded by [`HARD_MAX_ITERATIONS`] in preflight).
    pub budget: u32,
    /// Maximum wall-clock duration for the entire loop run.
    pub max_wall_clock: Duration,
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
            budget: DEFAULT_LOOP_BUDGET,
            max_wall_clock: Duration::from_secs(DEFAULT_MAX_WALL_CLOCK_SECS),
            stop_when_satisfied: true,
            satisfaction_threshold: 90,
            max_no_progress: 3,
            predicate: None,
        }
    }
}

/// Hard stops for loop iteration execution.
#[derive(Clone, Copy, Debug)]
pub struct SafetyCaps {
    /// Maximum worker iterations before capping/wrap-up.
    pub max_iterations: u32,
    /// Wall-clock budget for the whole run.
    pub max_wall_clock: Duration,
}

impl Default for SafetyCaps {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            max_wall_clock: Duration::from_secs(DEFAULT_MAX_WALL_CLOCK_SECS),
        }
    }
}

/// Decision from an [`IterationGate`] after one transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateOutcome {
    /// Stop the driver successfully.
    Stop {
        /// Reason recorded on [`hya_runtime_abi::DriverRunOutcome::Achieved`].
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

/// Independent stop authority for iterative loop runs.
#[async_trait]
pub trait IterationGate: Send + Sync {
    /// Judge whether to stop or continue.
    ///
    /// # Errors
    /// Return [`HostError`] to fail the run.
    async fn judge(
        &self,
        transcript: &str,
        cancel: &CancellationToken,
    ) -> Result<GateOutcome, HostError>;
}

/// Runs one worker iteration given a directive string.
#[async_trait]
pub trait IterationExecutor: Send + Sync {
    /// Execute one iteration and return a transcript for judging.
    ///
    /// # Errors
    /// Propagates turn/store failures.
    async fn run_iteration(
        &self,
        directive: &str,
        cancel: &CancellationToken,
    ) -> Result<String, HostError>;
}

/// Clamp a requested loop budget into the range [`validate_budget`] enforces
/// (`1..=HARD_MAX_ITERATIONS`).
#[must_use]
pub fn clamp_budget(requested: u32) -> u32 {
    requested.clamp(1, HARD_MAX_ITERATIONS)
}

/// Reject an unservable loop before any worker runs: budget must be within
/// `1..=HARD_MAX_ITERATIONS`.
///
/// # Errors
/// Returns [`HostError::InvalidInput`] if `config.budget` is `0` or exceeds
/// [`HARD_MAX_ITERATIONS`].
pub fn validate_budget(config: &LoopConfig) -> Result<(), HostError> {
    if config.budget == 0 || config.budget > HARD_MAX_ITERATIONS {
        return Err(HostError::InvalidInput(format!(
            "loop budget must be 1..={HARD_MAX_ITERATIONS}, got {}",
            config.budget
        )));
    }
    Ok(())
}

/// Wrap-up directive for the [`hya_runtime_abi::DriverRunOutcome::BudgetLimited`] pass:
/// no new substantive work, hand off verifiable state instead.
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
    let mut out = String::new();
    for message in &projection.session.messages {
        let mut text = String::new();
        for part in &message.parts {
            match part {
                PartProjection::Text { text: chunk, .. } => text.push_str(chunk),
                PartProjection::Tool { name, state, .. } => {
                    text.push_str(&format!("[tool:{name}:{state:?}]"));
                }
                PartProjection::Reasoning { .. } => {}
            }
        }
        out.push_str(&format!("[{:?}] {}\n", message.role, text));
    }
    out
}

/// Strip YAML frontmatter from a `SKILL.md` document, returning its trimmed body.
#[must_use]
pub fn skill_prompt_body(content: &str) -> &str {
    content
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n").map(|(_, body)| body.trim()))
        .unwrap_or(content)
}
