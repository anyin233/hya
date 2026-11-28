//! Iteration driver, child worker session executor, and the `hya/loop:loop` [`NativeDriver`]
//! implementation for `hya/loop`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hya_proto::{AgentName, ModelRef, SessionId};
use hya_runtime_abi::{
    DriverReport, DriverRequest, DriverRunOutcome, HostError, HostRuntime, NativeDriver,
    TurnSpawnRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::evaluators::{
    DEFAULT_LOOP_PLANNER_SYSTEM, DEFAULT_LOOP_VERIFIER_SYSTEM, HookLoopPlanner, HookLoopVerifier,
    ModelLoopPlanner, ModelLoopVerifier, RuntimeShouldStopHook,
};
use crate::gate::LoopGate;
use crate::predicate::LoopPredicate;
use crate::service::LoopService;
use crate::types::{
    DEFAULT_LOOP_BUDGET, DEFAULT_MAX_WALL_CLOCK_SECS, GateOutcome, HARD_MAX_ITERATIONS,
    IterationExecutor, IterationGate, LOOP_BUNDLE_ID, LOOP_DRIVER_ID, LoopConfig, LoopPlanner,
    LoopShouldStopHook, LoopVerifier, SafetyCaps, render_transcript, skill_prompt_body,
    validate_budget, wrap_up_directive,
};

/// Drives executor/gate pairs under [`SafetyCaps`].
pub struct IterationDriver {
    /// Caps applied each loop.
    pub caps: SafetyCaps,
}

impl IterationDriver {
    /// Create a driver with the given caps.
    #[must_use]
    pub fn new(caps: SafetyCaps) -> Self {
        Self { caps }
    }

    /// Run until the gate stops, a cap trips, or cancellation fires.
    ///
    /// # Errors
    /// Propagates executor/gate host errors.
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
                // pass demanding a handoff summary, then report `BudgetLimited`.
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

/// [`IterationExecutor`] that runs each loop step in a fresh child session
/// through [`HostRuntime::turn_runner`] and reads its transcript through
/// [`HostRuntime::session_store`].
pub struct WorkerSessionExecutor {
    runtime: Arc<dyn HostRuntime>,
    lead_session: SessionId,
    agent: AgentName,
    model: Option<ModelRef>,
    workdir: Option<PathBuf>,
}

impl WorkerSessionExecutor {
    /// Create a worker executor spawning child sessions under `lead_session`.
    #[must_use]
    pub fn new(
        runtime: Arc<dyn HostRuntime>,
        lead_session: SessionId,
        agent: AgentName,
        model: Option<ModelRef>,
        workdir: Option<PathBuf>,
    ) -> Self {
        Self {
            runtime,
            lead_session,
            agent,
            model,
            workdir,
        }
    }
}

#[async_trait]
impl IterationExecutor for WorkerSessionExecutor {
    async fn run_iteration(
        &self,
        directive: &str,
        cancel: &CancellationToken,
    ) -> Result<String, HostError> {
        let outcome = self
            .runtime
            .turn_runner()
            .spawn_session_and_run(
                TurnSpawnRequest {
                    parent: Some(self.lead_session),
                    agent: self.agent.clone(),
                    model: self.model.clone(),
                    workdir: self.workdir.clone(),
                    title: None,
                    prompt: directive.to_string(),
                    metadata: None,
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
            .read_projection(outcome.session)
            .await?;
        Ok(render_transcript(&projection))
    }
}

/// Drive loop iterations with the given gate/executor under safety caps.
///
/// # Errors
/// Propagates executor/gate host errors.
pub async fn drive_loop(
    executor: &dyn IterationExecutor,
    verifier: Arc<dyn LoopVerifier>,
    planner: Arc<dyn LoopPlanner>,
    target: String,
    config: LoopConfig,
    cancel: CancellationToken,
    should_stop: Option<Arc<dyn LoopShouldStopHook>>,
) -> Result<DriverRunOutcome, HostError> {
    let gate = LoopGate::new(target.clone(), verifier, planner, config.clone())
        .with_should_stop_hook(should_stop);
    let caps = SafetyCaps {
        max_iterations: config.budget,
        max_wall_clock: config.max_wall_clock,
    };
    IterationDriver::new(caps)
        .run(executor, &gate, target, cancel)
        .await
}

/// High-level loop mode entry: validate budget, create child-session worker
/// executor, and drive iterations until a terminal [`DriverRunOutcome`].
///
/// # Errors
/// Returns [`HostError::InvalidInput`] on out-of-range budget or propagates
/// host runtime errors.
#[allow(clippy::too_many_arguments)]
pub async fn run_loop(
    runtime: Arc<dyn HostRuntime>,
    lead_session: SessionId,
    agent: AgentName,
    model: Option<ModelRef>,
    workdir: Option<PathBuf>,
    target: String,
    verifier: Arc<dyn LoopVerifier>,
    planner: Arc<dyn LoopPlanner>,
    config: LoopConfig,
    cancel: CancellationToken,
    should_stop: Option<Arc<dyn LoopShouldStopHook>>,
) -> Result<DriverRunOutcome, HostError> {
    validate_budget(&config)?;
    let executor = WorkerSessionExecutor::new(runtime, lead_session, agent, model, workdir);
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

#[derive(Default, Deserialize)]
struct LoopDriverConfigPayload {
    #[serde(default)]
    evaluator_model: Option<String>,
    #[serde(default)]
    stop_when_satisfied: Option<bool>,
    #[serde(default)]
    workdir: Option<PathBuf>,
}

/// Native driver `hya/loop:loop`, behind `hya loop`.
pub struct NativeLoopDriver {
    service: Arc<LoopService>,
}

impl NativeLoopDriver {
    /// Create a driver backed by `service` for run health/metrics tracking.
    #[must_use]
    pub fn new(service: Arc<LoopService>) -> Self {
        Self { service }
    }
}

async fn resolve_skill_prompt(
    runtime: &Arc<dyn HostRuntime>,
    skill_id: &str,
    default_prompt: &str,
) -> String {
    if let Ok(Some(bundle)) = runtime.catalog_reader().get_bundle(LOOP_BUNDLE_ID).await
        && let Some(skill) = bundle
            .skills()
            .iter()
            .find(|resource| resource.local_id == skill_id)
    {
        return skill_prompt_body(&skill.content).to_string();
    }
    default_prompt.to_string()
}

fn outcome_label(outcome: &DriverRunOutcome) -> &'static str {
    match outcome {
        DriverRunOutcome::Achieved { .. } => "achieved",
        DriverRunOutcome::Capped { .. } => "capped",
        DriverRunOutcome::BudgetLimited { .. } => "budget_limited",
        DriverRunOutcome::Stalled { .. } => "stalled",
        DriverRunOutcome::InvalidCondition { .. } => "invalid_condition",
        DriverRunOutcome::Cancelled => "cancelled",
    }
}

/// One loop-mode run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopRunRequest {
    /// Lead session the loop drives.
    pub session: SessionId,
    /// Loop target.
    pub target: String,
    /// Iteration budget.
    pub budget: u32,
    /// Wall-clock cap in seconds (`0` uses the default).
    pub max_wall_clock_secs: u64,
    /// Verifier score that counts as satisfied.
    pub satisfaction_threshold: u8,
    /// Iterations without progress before the loop stalls.
    pub max_no_progress: u32,
    /// Deterministic `--while`/`--until` predicate (`{command, mode, workdir}`).
    pub predicate: Option<Value>,
    /// Worker agent.
    pub agent: Option<AgentName>,
    /// Worker model.
    pub model: Option<ModelRef>,
    /// `{evaluator_model?, stop_when_satisfied?, workdir?}`.
    pub config: Value,
}

/// Arguments of the `hya loop` CLI contribution.
#[derive(Deserialize)]
struct LoopArgs {
    target: String,
    #[serde(default)]
    budget: Option<u32>,
    #[serde(default)]
    max_iterations: Option<u32>,
    #[serde(default, rename = "while")]
    while_command: Option<String>,
    #[serde(default, rename = "until")]
    until_command: Option<String>,
    #[serde(default)]
    evaluator_model: Option<String>,
}

/// `loop.evaluator_model` (or a top-level `evaluator_model`) of the bundle's
/// own `config.yml`.
async fn configured_evaluator_model(runtime: &dyn HostRuntime) -> Option<String> {
    let config = runtime
        .catalog_reader()
        .bundle_config(LOOP_BUNDLE_ID)
        .await
        .ok()?;
    config
        .get("loop")
        .and_then(|section| section.get("evaluator_model"))
        .or_else(|| config.get("evaluator_model"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
}

#[async_trait]
impl NativeDriver for NativeLoopDriver {
    fn name(&self) -> &str {
        LOOP_DRIVER_ID
    }

    async fn run(
        &self,
        runtime: Arc<dyn HostRuntime>,
        request: DriverRequest,
        cancel: CancellationToken,
    ) -> Result<DriverReport, HostError> {
        let args: LoopArgs = serde_json::from_value(request.args)
            .map_err(|error| HostError::InvalidInput(format!("loop arguments: {error}")))?;
        let predicate = match (args.while_command, args.until_command) {
            (Some(_), Some(_)) => {
                return Err(HostError::InvalidInput(
                    "--while and --until are mutually exclusive; give exactly one loop condition"
                        .to_string(),
                ));
            }
            (Some(command), None) => Some(json!({
                "command": command, "mode": "while", "workdir": request.workdir,
            })),
            (None, Some(command)) => Some(json!({
                "command": command, "mode": "until", "workdir": request.workdir,
            })),
            (None, None) => None,
        };
        let budget = args
            .budget
            .or(args.max_iterations)
            .unwrap_or(DEFAULT_LOOP_BUDGET)
            .clamp(1, HARD_MAX_ITERATIONS);
        let evaluator_model = match args.evaluator_model {
            Some(model) => Some(model),
            None => configured_evaluator_model(runtime.as_ref()).await,
        };
        let run = LoopRunRequest {
            session: request.session,
            target: args.target,
            budget,
            max_wall_clock_secs: DEFAULT_MAX_WALL_CLOCK_SECS,
            satisfaction_threshold: 90,
            max_no_progress: 3,
            predicate,
            agent: request.agent,
            model: request.model,
            config: json!({
                "evaluator_model": evaluator_model,
                "stop_when_satisfied": true,
                "workdir": request.workdir,
            }),
        };
        let outcome = self.run_loop(runtime, run, cancel).await?;
        let mut report = DriverReport {
            stdout: format!("loop outcome: {outcome:?}\n"),
            ..DriverReport::default()
        };
        if let DriverRunOutcome::InvalidCondition { reason, .. } = &outcome {
            report.stderr = format!(
                "warning: the loop condition itself is broken, so this is NOT a success: {reason}\n\
                 Error: loop ended on a broken condition; not a success\n"
            );
            report.exit_code = 1;
        }
        Ok(report)
    }
}

impl NativeLoopDriver {
    /// Run one loop to its terminal outcome.
    ///
    /// # Errors
    /// Invalid budgets or predicates, and host failures.
    pub async fn run_loop(
        &self,
        runtime: Arc<dyn HostRuntime>,
        request: LoopRunRequest,
        cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError> {
        let extra: LoopDriverConfigPayload = if request.config.is_null() {
            LoopDriverConfigPayload::default()
        } else {
            serde_json::from_value(request.config.clone()).map_err(|error| {
                HostError::InvalidInput(format!("invalid loop driver config: {error}"))
            })?
        };
        let session_meta = runtime
            .session_store()
            .session_metadata(request.session)
            .await
            .ok()
            .flatten();
        let workdir = extra
            .workdir
            .or_else(|| {
                session_meta
                    .as_ref()
                    .and_then(|meta| meta.workdir.as_ref())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| PathBuf::from("."));
        let predicate = request
            .predicate
            .as_ref()
            .map(|value| LoopPredicate::from_json_value(value, Path::new(&workdir)))
            .transpose()?;
        let max_wall_clock_secs = if request.max_wall_clock_secs == 0 {
            DEFAULT_MAX_WALL_CLOCK_SECS
        } else {
            request.max_wall_clock_secs
        };
        let config = LoopConfig {
            budget: request.budget,
            max_wall_clock: Duration::from_secs(max_wall_clock_secs),
            stop_when_satisfied: extra.stop_when_satisfied.unwrap_or(true),
            satisfaction_threshold: request.satisfaction_threshold,
            max_no_progress: request.max_no_progress,
            predicate,
        };
        validate_budget(&config)?;

        let agent = request
            .agent
            .or_else(|| session_meta.as_ref().and_then(|meta| meta.agent.clone()))
            .unwrap_or_else(|| AgentName::new("hya-main"));
        let worker_model = request
            .model
            .or_else(|| session_meta.as_ref().and_then(|meta| meta.model.clone()));
        let evaluator_model = extra
            .evaluator_model
            .map(ModelRef::new)
            .or_else(|| worker_model.clone());

        let verifier_prompt = resolve_skill_prompt(
            &runtime,
            "loop-verifier-prompt",
            DEFAULT_LOOP_VERIFIER_SYSTEM,
        )
        .await;
        let planner_prompt =
            resolve_skill_prompt(&runtime, "loop-planner-prompt", DEFAULT_LOOP_PLANNER_SYSTEM)
                .await;

        let verifier_fallback: Arc<dyn LoopVerifier> = Arc::new(
            ModelLoopVerifier::new(Arc::clone(&runtime), evaluator_model.clone())
                .with_system_prompt(verifier_prompt),
        );
        let planner_fallback: Arc<dyn LoopPlanner> = Arc::new(
            ModelLoopPlanner::new(Arc::clone(&runtime), evaluator_model)
                .with_system_prompt(planner_prompt),
        );
        let verifier: Arc<dyn LoopVerifier> = Arc::new(HookLoopVerifier::new(
            Arc::clone(&runtime),
            request.session,
            verifier_fallback,
        ));
        let planner: Arc<dyn LoopPlanner> = Arc::new(HookLoopPlanner::new(
            Arc::clone(&runtime),
            request.session,
            planner_fallback,
        ));
        let should_stop: Arc<dyn LoopShouldStopHook> = Arc::new(RuntimeShouldStopHook::new(
            Arc::clone(&runtime),
            request.session,
        ));

        let _run_guard = self.service.enter_run();
        let _ = runtime
            .event_bus()
            .publish_extension(
                request.session,
                LOOP_BUNDLE_ID,
                "loop.started",
                json!({
                    "target": request.target,
                    "budget": config.budget,
                    "has_predicate": config.predicate.is_some(),
                }),
            )
            .await;

        let result = run_loop(
            Arc::clone(&runtime),
            request.session,
            agent,
            worker_model,
            Some(workdir),
            request.target,
            verifier,
            planner,
            config,
            cancel,
            Some(should_stop),
        )
        .await;

        if let Ok(ref outcome) = result {
            let _ = runtime
                .event_bus()
                .publish_extension(
                    request.session,
                    LOOP_BUNDLE_ID,
                    "loop.finished",
                    json!({
                        "outcome": outcome_label(outcome),
                    }),
                )
                .await;
        }
        result
    }
}
