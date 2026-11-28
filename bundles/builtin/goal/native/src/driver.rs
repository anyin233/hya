//! Goal-mode iteration driver, safety caps, contract validation, and ABI v2
//! [`GoalDriver`] implementation backed strictly by [`HostRuntime`].

use std::future::Future;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hya_proto::{AgentName, ModelRef, PartProjection, Projection, SessionId};
use hya_runtime_abi::{
    DriverRunOutcome, GoalDriver, GoalRunRequest, HostError, HostRuntime, TurnStartRequest,
};
use tokio_util::sync::CancellationToken;

use crate::evaluator::{
    DEFAULT_EVALUATOR_SYSTEM_PROMPT, GoalEvaluator, ModelGoalEvaluator, PluginGoalEvaluator,
    extract_config_string, resolve_evaluator_model, skill_prompt_body,
};
use crate::service::BUNDLE_ID;

/// Stable driver identifier registered by `hya/goal`.
pub const GOAL_DRIVER_ID: &str = "goal";

/// Hard stops for goal iteration drivers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SafetyCaps {
    /// Maximum worker iterations before capping / budget-limited wrap-up.
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

/// Decision from an [`IterationGate`] after one transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateOutcome {
    /// Stop the driver successfully.
    Stop {
        /// Reason recorded on [`DriverRunOutcome::Achieved`].
        reason: String,
    },
    /// Stop without achieving the target.
    Stalled,
    /// Stop because a deterministic condition is unusable.
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

/// Independent stop authority for iterative goal runs.
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

/// Runs one agent iteration given a directive string.
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

/// Drives executor/gate pairs under [`SafetyCaps`].
pub struct IterationDriver {
    /// Caps applied each iteration.
    pub caps: SafetyCaps,
    /// Behavioral stall guard: stop as [`DriverRunOutcome::Stalled`] when two
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

    /// Enable the behavioral stall guard (goal mode).
    #[must_use]
    pub fn with_stall_guard(mut self) -> Self {
        self.stall_guard = true;
        self
    }

    /// Run until the gate stops, a cap trips, or cancel fires.
    ///
    /// # Errors
    /// Propagates non-cancellation executor/gate errors.
    pub async fn run(
        &self,
        executor: &dyn IterationExecutor,
        gate: &dyn IterationGate,
        initial_directive: String,
        cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError> {
        let start = Instant::now();
        let mut directive = initial_directive;
        let mut iterations = 0u32;
        let mut previous_transcript: Option<String> = None;
        let mut previous_activity: Option<u64> = None;
        loop {
            if cancel.is_cancelled() {
                return Ok(DriverRunOutcome::Cancelled);
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
                    return Ok(DriverRunOutcome::Capped {
                        iterations,
                        which: which.to_string(),
                    });
                }
                // Budget exhaustion is not completion: run exactly one wrap-up
                // pass that demands a handoff summary, then report
                // `BudgetLimited` instead of forging an `Achieved`.
                match executor
                    .run_iteration(&wrap_up_directive(which), &cancel)
                    .await
                {
                    Err(HostError::Cancelled) => return Ok(DriverRunOutcome::Cancelled),
                    result => {
                        result?;
                    }
                }
                if cancel.is_cancelled() {
                    return Ok(DriverRunOutcome::Cancelled);
                }
                return Ok(DriverRunOutcome::BudgetLimited {
                    iterations,
                    which: which.to_string(),
                });
            }
            iterations += 1;
            let transcript = match executor.run_iteration(&directive, &cancel).await {
                Err(HostError::Cancelled) => return Ok(DriverRunOutcome::Cancelled),
                result => result?,
            };
            if cancel.is_cancelled() {
                return Ok(DriverRunOutcome::Cancelled);
            }
            // Stall guard (behavioral tripwire): fingerprint this iteration's
            // own activity — the transcript suffix appended over the previous
            // one. Two identical consecutive contributions mean the run is
            // repeating itself; stop instead of burning the budget.
            let activity = previous_transcript
                .as_ref()
                .and_then(|previous| transcript.strip_prefix(previous.as_str()))
                .map(str::to_string)
                .unwrap_or_else(|| transcript.clone());
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            activity.hash(&mut hasher);
            let activity_fp = hasher.finish();
            if self.stall_guard && previous_activity == Some(activity_fp) {
                return Ok(DriverRunOutcome::Stalled { iterations });
            }
            previous_activity = Some(activity_fp);
            previous_transcript = Some(transcript.clone());
            let judgment = match gate.judge(&transcript, &cancel).await {
                Err(HostError::Cancelled) => return Ok(DriverRunOutcome::Cancelled),
                result => result?,
            };
            if cancel.is_cancelled() {
                return Ok(DriverRunOutcome::Cancelled);
            }
            match judgment {
                GateOutcome::Stop { reason } => {
                    return Ok(DriverRunOutcome::Achieved { iterations, reason });
                }
                GateOutcome::Stalled => return Ok(DriverRunOutcome::Stalled { iterations }),
                GateOutcome::InvalidCondition { reason } => {
                    return Ok(DriverRunOutcome::InvalidCondition { iterations, reason });
                }
                GateOutcome::Continue { directive: next } => directive = next,
            }
        }
    }
}

/// Goal-condition contract sections (`/guided-goal` shape): when an author
/// structures a goal at all, these are the expected `##` sections.
const GOAL_CONTRACT_SECTIONS: [&str; 5] = [
    "objective",
    "success criteria",
    "verification",
    "boundaries",
    "stop conditions",
];

/// Validate the structural goal-condition contract.
///
/// Free-form conditions stay allowed — the checks apply only when the author
/// already uses `##` section structure:
/// - at least four of the five contract sections must be present;
/// - the `verification` section, when present, must contain an executable
///   signal: a fenced code block, a backtick-quoted command, or a `$ ` line.
///
/// # Errors
/// Returns [`HostError::InvalidInput`] with the missing sections / missing
/// verification signal.
pub fn validate_goal_condition(condition: &str) -> Result<(), HostError> {
    let section_line = |section: &str| {
        condition.lines().any(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("## ")
                && trimmed[3..]
                    .trim()
                    .to_ascii_lowercase()
                    .starts_with(section)
        })
    };
    let present = GOAL_CONTRACT_SECTIONS
        .iter()
        .filter(|section| section_line(section))
        .count();
    if present == 0 {
        return Ok(());
    }
    if present < 4 {
        return Err(HostError::InvalidInput(format!(
            "structured goal condition carries only {present}/5 contract sections \
             (objective, success criteria, verification, boundaries, stop conditions); \
             either complete the structure or drop the `##` headers"
        )));
    }
    let verification = condition
        .lines()
        .skip_while(|line| {
            !line
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("## verification")
        })
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with("## "))
        .collect::<Vec<_>>()
        .join("\n");
    let has_executable_signal = verification.contains("```")
        || verification.contains('`')
        || verification
            .lines()
            .any(|line| line.trim_start().starts_with("$ "));
    if !has_executable_signal {
        return Err(HostError::InvalidInput(
            "structured goal condition has a `## Verification` section without an \
             executable signal; add the exact command (fenced block, backticks, or a \
             `$ ` line)"
                .to_string(),
        ));
    }
    Ok(())
}

/// Wrap-up directive for the [`DriverRunOutcome::BudgetLimited`] pass: no new
/// substantive work, hand off state instead.
#[must_use]
pub fn wrap_up_directive(which: &str) -> String {
    format!(
        "BUDGET LIMIT REACHED ({which}). Do not start any new substantive work. \
         Wrap up now: report what was completed, the current verifiable state, \
         and the remaining work as a handoff. Budget exhaustion is not completion."
    )
}

/// [`IterationGate`] that wraps a [`GoalEvaluator`].
pub struct GoalGate {
    condition: String,
    evaluator: Arc<dyn GoalEvaluator>,
}

impl GoalGate {
    /// Construct a new [`GoalGate`] for `condition` backed by `evaluator`.
    #[must_use]
    pub fn new(condition: String, evaluator: Arc<dyn GoalEvaluator>) -> Self {
        Self {
            condition,
            evaluator,
        }
    }
}

#[async_trait]
impl IterationGate for GoalGate {
    async fn judge(
        &self,
        transcript: &str,
        cancel: &CancellationToken,
    ) -> Result<GateOutcome, HostError> {
        let verdict = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(HostError::Cancelled),
            verdict = self.evaluator.evaluate(&self.condition, transcript, cancel) => verdict?,
        };
        if verdict.met {
            Ok(GateOutcome::Stop {
                reason: verdict.reason,
            })
        } else {
            Ok(GateOutcome::Continue {
                directive: format!(
                    "{}\n\nThe goal is not yet met: {}\nContinue working toward it.",
                    self.condition, verdict.reason
                ),
            })
        }
    }
}

/// [`IterationExecutor`] that runs one lead turn via [`HostRuntime::turn_runner`]
/// and renders the session transcript from [`HostRuntime::session_store`].
pub struct LeadTurnExecutor {
    runtime: Arc<dyn HostRuntime>,
    session: SessionId,
    agent: Option<AgentName>,
    model: Option<ModelRef>,
}

impl LeadTurnExecutor {
    /// Construct a lead-turn executor bound to `runtime` and `session`.
    #[must_use]
    pub fn new(
        runtime: Arc<dyn HostRuntime>,
        session: SessionId,
        agent: Option<AgentName>,
        model: Option<ModelRef>,
    ) -> Self {
        Self {
            runtime,
            session,
            agent,
            model,
        }
    }
}

#[async_trait]
impl IterationExecutor for LeadTurnExecutor {
    async fn run_iteration(
        &self,
        directive: &str,
        cancel: &CancellationToken,
    ) -> Result<String, HostError> {
        let outcome = self
            .runtime
            .turn_runner()
            .run_turn(
                TurnStartRequest {
                    session: self.session,
                    prompt: directive.to_string(),
                    agent: self.agent.clone(),
                    model: self.model.clone(),
                    system_override: None,
                    metadata: None,
                    actor_claim: None,
                },
                cancel.clone(),
            )
            .await?;
        if let Some(error) = outcome.error {
            return Err(HostError::Internal(error));
        }
        let projection = self
            .runtime
            .session_store()
            .read_projection(self.session)
            .await?;
        Ok(render_transcript(&projection))
    }
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
                // Tool activity is part of the iteration fingerprint: two
                // rounds running the identical tools with identical inputs
                // must render identical transcripts (stall guard).
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

/// Run goal mode: loop the lead session until the independent evaluator reports
/// the condition met, or a safety cap trips.
///
/// # Errors
/// Propagates non-cancellation host runtime or evaluator errors.
#[allow(clippy::too_many_arguments)]
pub async fn run_goal(
    runtime: Arc<dyn HostRuntime>,
    session: SessionId,
    agent: Option<AgentName>,
    model: Option<ModelRef>,
    condition: String,
    evaluator: Arc<dyn GoalEvaluator>,
    caps: SafetyCaps,
    cancel: CancellationToken,
) -> Result<DriverRunOutcome, HostError> {
    let executor = LeadTurnExecutor::new(runtime, session, agent, model);
    let gate = GoalGate::new(condition.clone(), evaluator);
    IterationDriver::new(caps)
        .with_stall_guard()
        .run(&executor, &gate, condition, cancel)
        .await
}

async fn resolve_evaluator_system_prompt(runtime: &dyn HostRuntime) -> String {
    for bundle_id in [BUNDLE_ID, "hya/goal-loop"] {
        if let Ok(Some(bundle)) = runtime.catalog_reader().get_bundle(bundle_id).await
            && let Some(skill) = bundle
                .skills()
                .iter()
                .find(|skill| skill.local_id == "evaluator-prompt")
        {
            return skill_prompt_body(&skill.content).to_string();
        }
    }
    DEFAULT_EVALUATOR_SYSTEM_PROMPT.to_string()
}

async fn resolve_configured_evaluator_model(runtime: &dyn HostRuntime) -> Option<String> {
    for bundle_id in [BUNDLE_ID, "hya/goal-loop"] {
        if let Ok(config) = runtime.catalog_reader().bundle_config(bundle_id).await
            && let Some(model) = extract_config_string(&config, "evaluator_model")
        {
            return Some(model);
        }
    }
    None
}

/// Bind goal evaluator resources through `runtime` and prepare the cancellable
/// goal driver future.
///
/// # Errors
/// Returns [`HostError::InvalidInput`] if a structured goal condition violates
/// the goal contract.
pub async fn prepare_goal(
    runtime: Arc<dyn HostRuntime>,
    request: GoalRunRequest,
    cancel: CancellationToken,
) -> Result<impl Future<Output = Result<DriverRunOutcome, HostError>> + use<>, HostError> {
    validate_goal_condition(&request.goal)?;

    let cli_model = extract_config_string(&request.config, "evaluator_model");
    let config_model = resolve_configured_evaluator_model(runtime.as_ref()).await;
    let default_model = extract_config_string(&request.config, "default_model")
        .or_else(|| request.model.as_ref().map(|m| m.as_str().to_string()));
    let evaluator_model = resolve_evaluator_model(
        cli_model.as_deref(),
        config_model.as_deref(),
        default_model.as_deref(),
    )
    .map(ModelRef::new);

    let system_prompt = resolve_evaluator_system_prompt(runtime.as_ref()).await;
    let fallback: Arc<dyn GoalEvaluator> = Arc::new(
        ModelGoalEvaluator::new(Arc::clone(&runtime), evaluator_model)
            .with_system_prompt(system_prompt),
    );
    let has_hook = runtime
        .extension_registry()
        .has_hook(Some(request.session), crate::evaluator::EVALUATE_HOOK)
        .await
        .unwrap_or(false);
    let evaluator: Arc<dyn GoalEvaluator> = if has_hook {
        Arc::new(
            PluginGoalEvaluator::new(Arc::clone(&runtime), Some(request.session))
                .with_fallback(Arc::clone(&fallback)),
        )
    } else {
        fallback
    };

    let caps = SafetyCaps {
        max_iterations: request.max_iterations,
        max_wall_clock: if request.max_wall_clock_secs == 0 {
            SafetyCaps::default().max_wall_clock
        } else {
            Duration::from_secs(request.max_wall_clock_secs)
        },
    };

    Ok(run_goal(
        runtime,
        request.session,
        request.agent,
        request.model,
        request.goal,
        evaluator,
        caps,
        cancel,
    ))
}

/// Native ABI v2 [`GoalDriver`] exported by `hya/goal`.
#[derive(Debug, Default)]
pub struct NativeGoalDriver;

#[async_trait]
impl GoalDriver for NativeGoalDriver {
    fn id(&self) -> &str {
        GOAL_DRIVER_ID
    }

    async fn run_goal(
        &self,
        runtime: Arc<dyn HostRuntime>,
        request: GoalRunRequest,
        cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError> {
        prepare_goal(runtime, request, cancel).await?.await
    }
}
