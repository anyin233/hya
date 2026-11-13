//! Workflow execution over the governed subagent team primitives.
//!
//! Ready Stages register parked resident actors and activate them through the
//! episode API as soon as their predecessors complete. The controller accepts at-rest settlement or
//! a terminal report as Stage evidence. Loop continuations reactivate the same
//! actor; each independent verifier runs a fresh, capacity-counted episode.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use hya_proto::{
    ActorClaim, Event, MemberId, ModelRef, ReportOutcome, SessionId, WorkflowMemberRole,
    WorkflowRunId, WorkflowStageStatus,
};
use hya_provider::ReasoningEffort;
use hya_tool::AgentDef;
use hya_workflow::{
    CompiledWorkflow, FailurePolicy, MAX_PREDECESSOR_OUTPUT_BYTES, StageEvidence,
    StageEvidenceStatus, StageMode, VerifySpec, WorkflowModelAssignment, WorkflowStage,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::AgentResourcePolicy;
use crate::completion::{
    GateOutcome, IterationDriver, IterationExecutor, IterationGate, SafetyCaps,
};
use crate::engine::{AgentSpec, SessionEngine};
use crate::error::CoreError;
use crate::resident::{ActivationResult, ResidentSupervisor};
use crate::sidecar::BoundSidecarFactory;

use super::{
    WorkflowError, WorkflowModelRoute, WorkflowRouteRecord, WorkflowRouteRecorder,
    WorkflowRoutingContext, WorkflowTurnRoute, WorkflowTurnRouteSpec,
};
/// Per-run inputs for executing a workflow.
/// One value per run; everything here comes from the calling surface (CLI,
/// tool plane) which owns the session binding and caller authorization. Each
/// member's own execution context (spawnable roster, resource policy, sidecar
/// factory) is derived from the STAGE agent's definition through the same
/// engine accessors the task-tool spawn path uses — never inherited from the
/// caller — so a stage can only delegate further targets listed in its own
/// `can_spawn` roster.
#[derive(Clone)]
pub struct WorkflowRunContext {
    /// Immutable runtime snapshot binding used to resolve stage agents.
    pub binding: crate::runtime_registry::TurnBinding,
    /// Caller agent id whose `can_spawn` roster authorizes every stage agent.
    pub caller: String,
    /// Base agent spec for member derivation (model fallback context).
    pub base_agent: AgentSpec,
    /// Values for declared workflow inputs. Every declared key must be present.
    pub inputs: BTreeMap<String, String>,
    /// Resident scheduling owner required only when the compiled plan has actors.
    pub resident_supervisor: Option<Arc<ResidentSupervisor>>,
    /// Configured category/provider context for explicit Stage route admission.
    /// `None` preserves direct core callers and the old no-assignment path.
    pub routing: Option<WorkflowRoutingContext>,
}
/// Terminal status of one workflow stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    /// The Stage has not started.
    Pending,
    /// The Stage finished successfully.
    Done,
    /// The Stage finished with an execution failure.
    Failed,
    /// The Stage or run was cancelled.
    Cancelled,
    /// Failure policy or an unsuccessful predecessor prevented activation.
    Skipped,
}

impl std::fmt::Display for StageStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Done => write!(f, "done"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Skipped => write!(f, "skipped"),
        }
    }
}

/// Lead-visible result of one stage.
#[derive(Clone, Debug, Serialize)]
pub struct StageReport {
    /// Stage id from the definition.
    pub stage: String,
    /// Resolved agent id that ran the stage.
    pub agent: String,
    /// Terminal member status.
    pub status: StageStatus,
    /// Child session carrying the transcript, when the member ran.
    pub session: Option<String>,
    /// Bounded terminal output or failure detail; empty while pending/skipped.
    pub output: String,
}

/// Overall terminal state of a workflow run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    /// Every Stage of the DAG ran successfully.
    Completed,
    /// At least one Stage failed.
    Failed,
    /// The run was cancelled.
    Cancelled,
}

impl std::fmt::Display for WorkflowStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Result of a full workflow execution: overall status plus one row per stage
/// that ran, in declaration order.
#[derive(Clone, Debug, Serialize)]
pub struct WorkflowRunReport {
    /// Overall terminal state.
    pub status: WorkflowStatus,
    /// Reports for stages that ran; absent stages did not start.
    pub stages: Vec<StageReport>,
}

/// Execution context of one stage agent, computed up front so authorization
/// failures surface before any member spawns. Mirrors
/// `resolve_authorized_spawn_member` in the task-tool path: the target's own
/// spec, spawnable roster, resource policy, and bound sidecar factory.
#[derive(Clone)]
struct ResolvedAgent {
    spec: AgentSpec,
    agents: Arc<[AgentDef]>,
    resources: AgentResourcePolicy,
    sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
    route: Option<WorkflowModelRoute>,
}

#[derive(Clone)]
struct ResidentActor {
    session: SessionId,
    handle: String,
    member: MemberId,
}

struct ResidentActivation {
    actor: ResidentActor,
    directive: String,
    system_context: Arc<str>,
    cancel: CancellationToken,
    actor_claim: Option<ActorClaim>,
    route: Option<WorkflowTurnRoute>,
}

/// Fully preflighted Workflow execution.
///
/// Preflight creates no child Session or mail and reserves no live capacity.
pub struct PreparedWorkflowRun {
    engine: Arc<SessionEngine>,
    lead: SessionId,
    workflow: CompiledWorkflow,
    context: WorkflowRunContext,
    resolved: Vec<ResolvedAgent>,
    verifiers: Vec<Option<ResolvedAgent>>,
    durable_run: Option<WorkflowRunId>,
    actor_claim: Option<ActorClaim>,
}

fn resolve_agent(
    engine: &SessionEngine,
    ctx: &WorkflowRunContext,
    agent_id: &str,
    workflow: &str,
    role: &str,
    assignment: Option<&WorkflowModelAssignment>,
) -> Result<ResolvedAgent, WorkflowError> {
    let definition = ctx
        .binding
        .resolve_spawn(&ctx.caller, agent_id)
        .map_err(|error| WorkflowError::Unauthorized {
            caller: ctx.caller.clone(),
            agent_id: agent_id.to_string(),
            detail: error.to_string(),
        })?;
    let mut spec =
        engine.agent_spec_for_binding(&ctx.binding, &ctx.base_agent, definition.stable_id)?;
    let router = ctx
        .routing
        .as_ref()
        .map(|routing| Arc::clone(&routing.router))
        .unwrap_or_else(|| engine.provider_router());
    let is_servable = |model: &ModelRef| router.resolve(model).is_some();
    spec = crate::category::apply_agent_model_preference(
        spec,
        &definition,
        ctx.binding.agent_model_preference(definition.stable_id),
        &is_servable,
    );
    if let Some(routing) = ctx.routing.as_ref() {
        let member = hya_tool::SpawnMember::default();
        let is_servable = |model: &ModelRef| routing.router.resolve(model).is_some();
        spec = crate::category::apply_spawn_model_policy(
            spec,
            &definition,
            &member,
            &routing.categories,
            &is_servable,
        );
    }
    let route = assignment
        .map(|assignment| resolve_model_route(workflow, agent_id, role, assignment, &router))
        .transpose()?;
    if let Some(route) = &route {
        let selected = route.selected().ok_or_else(|| WorkflowError::Invalid {
            workflow: workflow.to_string(),
            detail: format!("Stage `{agent_id}` {role} route selected no candidate"),
        })?;
        // An explicit assignment replaces inherited Agent effort. Keep the
        // effective value on the child spec as well as the request-local route.
        spec.model = selected.model.clone();
        spec.reasoning = Some(selected.reasoning);
    }
    // The member's OWN reachable roster comes from the TARGET agent's
    // `can_spawn`, not the caller's; this bounds what the stage can itself
    // delegate during its turn to exactly what the task path would grant.
    let unauthorized = |detail: String| WorkflowError::Unauthorized {
        caller: ctx.caller.clone(),
        agent_id: agent_id.to_string(),
        detail,
    };
    let agents = engine
        .agent_roster_for_binding(&ctx.binding, definition.stable_id)
        .map_err(|error| unauthorized(error.to_string()))?;
    let resources = engine
        .agent_resource_policy_for_binding(&ctx.binding, definition.stable_id)
        .map_err(|error| unauthorized(error.to_string()))?;
    let sidecar_factory = match engine.sidecar_environment() {
        Some(environment) => environment
            .factory_for(&ctx.binding, definition.stable_id)
            .map_err(|error| unauthorized(error.to_string()))?,
        None => None,
    };
    Ok(ResolvedAgent {
        spec,
        agents,
        resources,
        sidecar_factory,
        route,
    })
}

/// Resolve one authored assignment to typed efforts and an immutable start index.
fn resolve_model_route(
    workflow: &str,
    stage: &str,
    role: &str,
    assignment: &WorkflowModelAssignment,
    router: &hya_provider::ProviderRouter,
) -> Result<WorkflowModelRoute, WorkflowError> {
    let mut candidates = Vec::with_capacity(1 + assignment.fallback().len());
    let mut seen = BTreeSet::new();
    let entries = std::iter::once((assignment.id(), assignment.reasoning())).chain(
        assignment
            .fallback()
            .iter()
            .map(|candidate| (candidate.id(), candidate.reasoning())),
    );
    for (entry_index, (model_id, authored_reasoning)) in entries.enumerate() {
        if model_id.contains('#') || model_id.trim().is_empty() {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!(
                    "Stage `{stage}` {role} route has invalid model id at index {entry_index}"
                ),
            });
        }
        let model = ModelRef::new(model_id);
        let reasoning = match authored_reasoning {
            Some(label) => ReasoningEffort::parse(label).ok_or_else(|| WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!(
                    "Stage `{stage}` {role} route has unknown reasoning effort `{label}` at index {entry_index}"
                ),
            })?,
            None => router
                .reasoning_default(&model)
                .unwrap_or(ReasoningEffort::Off),
        };
        if !seen.insert((model.to_string(), reasoning)) {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!(
                    "Stage `{stage}` {role} route contains duplicate effective candidate `{model_id}` / `{}`",
                    reasoning.as_str()
                ),
            });
        }
        if let Some(supported) = router.supports_reasoning_effort(&model, reasoning) {
            if !supported {
                return Err(WorkflowError::Invalid {
                    workflow: workflow.to_string(),
                    detail: format!(
                        "Stage `{stage}` {role} candidate `{model_id}` does not support reasoning `{}`",
                        reasoning.as_str()
                    ),
                });
            }
        } else if reasoning != ReasoningEffort::Off
            && router
                .capabilities(&model)
                .is_some_and(|caps| !caps.reasoning_request)
        {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!(
                    "Stage `{stage}` {role} candidate `{model_id}` does not support reasoning requests"
                ),
            });
        }
        candidates.push(crate::workflow::WorkflowModelRouteCandidate { model, reasoning });
    }
    let Some(selected_index) = candidates
        .iter()
        .position(|candidate| router.resolve(&candidate.model).is_some())
    else {
        return Err(WorkflowError::Invalid {
            workflow: workflow.to_string(),
            detail: format!("Stage `{stage}` {role} route has no routable candidate"),
        });
    };
    Ok(WorkflowModelRoute {
        candidates: candidates.into(),
        selected_index,
    })
}

/// Preflight and reserve one compiled Workflow before any child or mail effect.
///
/// # Errors
/// Returns validation, authorization, semantic, lineage, or admission failures.
pub async fn prepare_workflow_run(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    workflow: &CompiledWorkflow,
    context: WorkflowRunContext,
    durable_run: Option<WorkflowRunId>,
) -> Result<PreparedWorkflowRun, WorkflowError> {
    prepare_workflow_run_for_actor(engine, lead, workflow, context, durable_run, None).await
}

/// Preflight a Workflow while retaining an optional actor capability for every
/// durable child and lifecycle mutation.
///
/// # Errors
/// Returns the same validation, authorization, semantic, lineage, or admission
/// failures as [`prepare_workflow_run`].
pub async fn prepare_workflow_run_for_actor(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    workflow: &CompiledWorkflow,
    context: WorkflowRunContext,
    durable_run: Option<WorkflowRunId>,
    actor_claim: Option<ActorClaim>,
) -> Result<PreparedWorkflowRun, WorkflowError> {
    workflow.validate_inputs(&context.inputs)?;
    let plan = workflow.plan();
    let name = workflow.definition().name();
    let wanted = worst_case_admission_units(plan.stages());

    let mut resolved = Vec::with_capacity(plan.stages().len());
    let mut verifiers = Vec::with_capacity(plan.stages().len());
    for stage in plan.stages() {
        resolved.push(resolve_agent(
            &engine,
            &context,
            stage.agent(),
            name,
            "worker",
            stage.model(),
        )?);
        verifiers.push(
            stage
                .verify()
                .map(|verify| {
                    resolve_agent(
                        &engine,
                        &context,
                        verify.agent(),
                        name,
                        "verifier",
                        verify.model(),
                    )
                })
                .transpose()?,
        );
    }
    validate_resolved_semantics(name, plan.stages(), &resolved, &context)?;
    preflight_workflow_capacity(&engine, lead, name, wanted).await?;
    Ok(PreparedWorkflowRun {
        engine,
        lead,
        workflow: workflow.clone(),
        context,
        resolved,
        verifiers,
        durable_run,
        actor_claim,
    })
}

/// Execute `workflow` under `lead` after shared preflight.
///
/// # Errors
/// Returns validation, authorization, admission, or execution failures.
pub async fn run_workflow(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    workflow: &CompiledWorkflow,
    context: WorkflowRunContext,
    cancel: CancellationToken,
) -> Result<WorkflowRunReport, WorkflowError> {
    prepare_workflow_run(engine, lead, workflow, context, None)
        .await?
        .execute(cancel)
        .await
}

struct RoutePersistence {
    recorder: Option<WorkflowRouteRecorder>,
    task: Option<tokio::task::JoinHandle<Result<(), CoreError>>>,
}

impl RoutePersistence {
    fn start(engine: Arc<SessionEngine>, lead: SessionId, actor_claim: Option<ActorClaim>) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let recorder = WorkflowRouteRecorder::new(sender);
        let task = tokio::spawn(async move {
            persist_route_records(engine, lead, actor_claim, receiver).await
        });
        Self {
            recorder: Some(recorder),
            task: Some(task),
        }
    }

    fn recorder(&self) -> Option<WorkflowRouteRecorder> {
        self.recorder.clone()
    }

    async fn finish(mut self) -> Result<(), WorkflowError> {
        self.recorder.take();
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        task.await
            .map_err(|error| WorkflowError::Invalid {
                workflow: "route persistence".to_string(),
                detail: format!("route outcome persistence task failed: {error}"),
            })?
            .map_err(WorkflowError::Engine)
    }
}

impl Drop for RoutePersistence {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn persist_route_records(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    actor_claim: Option<ActorClaim>,
    mut receiver: tokio::sync::mpsc::Receiver<WorkflowRouteRecord>,
) -> Result<(), CoreError> {
    while let Some(record) = receiver.recv().await {
        let result = engine
            .record_workflow_event_for_actor(actor_claim.as_ref(), lead, record.event)
            .await;
        let acknowledgement = result.as_ref().map(|_| ()).map_err(ToString::to_string);
        let _ = record.ack.send(acknowledgement);
        result?;
    }
    Ok(())
}

/// Execute the already-validated graph; registrations consume live capacity.
///
/// # Errors
/// Returns rendering or governed Stage execution failures.
impl PreparedWorkflowRun {
    /// Return the prepared worker route for a Stage, when explicitly assigned.
    #[must_use]
    pub fn worker_route(&self, stage_index: usize) -> Option<&WorkflowModelRoute> {
        self.resolved.get(stage_index)?.route.as_ref()
    }

    /// Return the prepared verifier route for a Stage, when explicitly assigned.
    #[must_use]
    pub fn verifier_route(&self, stage_index: usize) -> Option<&WorkflowModelRoute> {
        self.verifiers
            .get(stage_index)?
            .as_ref()
            .and_then(|resolved| resolved.route.as_ref())
    }

    /// Whether any Stage has an explicit worker or verifier route.
    #[must_use]
    pub fn has_explicit_routes(&self) -> bool {
        self.resolved
            .iter()
            .any(|resolved| resolved.route.is_some())
            || self
                .verifiers
                .iter()
                .flatten()
                .any(|resolved| resolved.route.is_some())
    }
    /// Execute the already-validated graph; registrations consume live capacity.
    ///
    /// # Errors
    /// Returns rendering or governed Stage execution failures.
    pub async fn execute(
        self,
        cancel: CancellationToken,
    ) -> Result<WorkflowRunReport, WorkflowError> {
        let Self {
            engine,
            lead,
            workflow,
            context,
            resolved,
            verifiers,
            durable_run,
            actor_claim,
        } = self;
        let _activity = match context.resident_supervisor.as_ref() {
            Some(supervisor) => Some(supervisor.root_driver_activity(lead).await?),
            None => None,
        };
        let plan = workflow.plan();
        let name = workflow.definition().name();
        let mut reports: Vec<StageReport> = plan.stages().iter().map(pending_report).collect();
        let mut actors: BTreeMap<String, ResidentActor> = BTreeMap::new();
        let has_explicit_routes = resolved.iter().any(|agent| agent.route.is_some())
            || verifiers
                .iter()
                .flatten()
                .any(|agent| agent.route.is_some());
        let route_persistence = (durable_run.is_some() && has_explicit_routes)
            .then(|| RoutePersistence::start(engine.clone(), lead, actor_claim));
        let route_recorder = route_persistence
            .as_ref()
            .and_then(RoutePersistence::recorder);
        let mut registered = Vec::new();
        let run_cancel = cancel.child_token();
        let mut running = futures::stream::FuturesUnordered::new();
        let result = async {
            let mut remaining = plan
                .stages()
                .iter()
                .map(|stage| stage.predecessor_indices().len())
                .collect::<Vec<_>>();
            let mut successors = vec![Vec::new(); reports.len()];
            for (index, stage) in plan.stages().iter().enumerate() {
                for &predecessor in stage.predecessor_indices() {
                    successors[predecessor].push(index);
                }
            }
            let mut ready = remaining
                .iter()
                .enumerate()
                .filter_map(|(index, &count)| (count == 0).then_some(index))
                .collect::<BTreeSet<_>>();
            let mut busy_actors = BTreeSet::new();
            let mut failed = false;
            loop {
                let admit = !(run_cancel.is_cancelled()
                    || failed && workflow.definition().on_failure() == FailurePolicy::FailFast);
                if admit {
                    // A ready actor is reserved through its complete Stage (including
                    // verification). Compiled indices break ties between ready uses.
                    let candidates = ready.iter().copied().collect::<Vec<_>>();
                    for index in candidates {
                        if run_cancel.is_cancelled() {
                            break;
                        }
                        let stage = &plan.stages()[index];
                        if stage
                            .actor()
                            .is_some_and(|actor| busy_actors.contains(actor))
                        {
                            continue;
                        }
                        ready.remove(&index);
                        if stage
                            .predecessor_indices()
                            .iter()
                            .any(|&predecessor| reports[predecessor].status != StageStatus::Done)
                        {
                            reports[index].status = StageStatus::Skipped;
                            for &successor in &successors[index] {
                                remaining[successor] -= 1;
                                if remaining[successor] == 0 {
                                    ready.insert(successor);
                                }
                            }
                            continue;
                        }
                        let stage_evidence =
                            reports.iter().map(report_evidence).collect::<Vec<_>>();
                        let rendered =
                            workflow.render_stage(index, &context.inputs, &stage_evidence)?;
                        if let Some(run) = durable_run {
                            engine
                                .record_workflow_event_for_actor(
                                    actor_claim.as_ref(),
                                    lead,
                                    Event::WorkflowStageStarted {
                                        session: lead,
                                        run,
                                        stage: stage.id().to_string(),
                                    },
                                )
                                .await?;
                        }
                        let supervisor = context.resident_supervisor.as_ref().ok_or_else(|| {
                            WorkflowError::Invalid {
                                workflow: name.to_string(),
                                detail: "resident supervisor disappeared after preflight".into(),
                            }
                        })?;
                        let existing = stage.actor().and_then(|key| actors.get(key)).cloned();
                        let actor = if let Some(actor) = existing {
                            actor
                        } else {
                            let worker = &resolved[index];
                            let label = match stage.actor() {
                                Some(key) => format!("Workflow {name} resident actor `{key}`"),
                                None => format!("Workflow {name} / stage {}", stage.id()),
                            };
                            let (session, handle, member) = supervisor
                                .spawn_resident_parked(
                                    lead,
                                    worker.spec.clone(),
                                    (
                                        context.binding.clone(),
                                        worker.agents.clone(),
                                        worker.resources.clone(),
                                        worker.sidecar_factory.clone(),
                                    ),
                                    label,
                                    actor_claim.as_ref(),
                                    Some(Arc::from(rendered.system_context())),
                                )
                                .await?;
                            let actor = ResidentActor {
                                session,
                                handle,
                                member,
                            };
                            // Keep each receipt before any further fallible operation.
                            registered.push(actor.clone());
                            if let Some(key) = stage.actor() {
                                actors.insert(key.to_string(), actor.clone());
                            }
                            actor
                        };
                        if let Some(run) = durable_run {
                            engine
                                .record_workflow_event_for_actor(
                                    actor_claim.as_ref(),
                                    lead,
                                    Event::WorkflowStageMemberLinked {
                                        session: lead,
                                        run,
                                        stage: stage.id().to_string(),
                                        member: actor.member,
                                        role: WorkflowMemberRole::Worker,
                                        iteration: 0,
                                    },
                                )
                                .await?;
                        }
                        if let Some(key) = stage.actor() {
                            busy_actors.insert(key);
                        }
                        let route = resolved[index].route.clone().map(|route| {
                            WorkflowTurnRoute::new(WorkflowTurnRouteSpec {
                                route,
                                session: lead,
                                run: durable_run,
                                stage: stage.id().to_string(),
                                member: actor.member,
                                role: WorkflowMemberRole::Worker,
                                iteration: 0,
                                recorder: route_recorder.clone(),
                            })
                        });
                        let activation = ResidentActivation {
                            actor: actor.clone(),
                            directive: rendered.directive().to_string(),
                            system_context: Arc::from(rendered.system_context()),
                            cancel: run_cancel.child_token(),
                            actor_claim,
                            route,
                        };
                        let engine = engine.clone();
                        let supervisor = supervisor.clone();
                        let context = &context;
                        let resolved = &resolved;
                        let verifiers = &verifiers;
                        let run_cancel = run_cancel.clone();
                        let route_recorder = route_recorder.clone();
                        running.push(async move {
                            let mut report = activate_resident_stage(
                                engine.clone(),
                                lead,
                                supervisor,
                                stage,
                                activation,
                            )
                            .await?;
                            if let Some((verify, verifier)) =
                                stage.verify().zip(verifiers[index].clone())
                                && report.status == StageStatus::Done
                            {
                                match drive_loop_stage(
                                    engine,
                                    lead,
                                    name,
                                    context,
                                    stage,
                                    verify,
                                    rendered.directive(),
                                    rendered.system_context(),
                                    rendered
                                        .verification_condition()
                                        .unwrap_or_else(|| verify.until()),
                                    actor,
                                    &resolved[index],
                                    verifier,
                                    report.clone(),
                                    run_cancel.clone(),
                                    actor_claim,
                                    durable_run,
                                    route_recorder,
                                )
                                .await
                                {
                                    Ok(completed) => report = completed,
                                    Err(WorkflowError::Engine(CoreError::Store(
                                        error @ hya_store::StoreError::CapacityExceeded { .. },
                                    ))) => {
                                        return Err(CoreError::Store(error).into());
                                    }
                                    Err(error) => {
                                        report.status = if run_cancel.is_cancelled() {
                                            StageStatus::Cancelled
                                        } else {
                                            StageStatus::Failed
                                        };
                                        report.output =
                                            clamp(format!("loop Stage failed: {error}"));
                                    }
                                }
                            }
                            Ok::<_, WorkflowError>((index, report))
                        });
                    }
                }
                let Some(result) = running.next().await else {
                    // Skipping a blocked Stage can expose another blocked successor.
                    if admit && !ready.is_empty() {
                        continue;
                    }
                    break;
                };
                let (index, report) = result?;
                if let Some(key) = plan.stages()[index].actor() {
                    busy_actors.remove(key);
                }
                failed |= report.status == StageStatus::Failed;
                if let Some(run) = durable_run {
                    engine
                        .record_workflow_event_for_actor(
                            actor_claim.as_ref(),
                            lead,
                            Event::WorkflowStageFinished {
                                session: lead,
                                run,
                                stage: report.stage.clone(),
                                status: protocol_stage_status(report.status),
                            },
                        )
                        .await?;
                }
                reports[index] = report;
                for &successor in &successors[index] {
                    remaining[successor] -= 1;
                    if remaining[successor] == 0 {
                        ready.insert(successor);
                    }
                }
            }
            if run_cancel.is_cancelled() {
                mark_pending(&mut reports, StageStatus::Cancelled);
                return Ok(WorkflowRunReport {
                    status: WorkflowStatus::Cancelled,
                    stages: reports,
                });
            }
            mark_pending(&mut reports, StageStatus::Skipped);

            let status = if reports
                .iter()
                .any(|report| report.status == StageStatus::Failed)
            {
                WorkflowStatus::Failed
            } else {
                WorkflowStatus::Completed
            };
            drop(route_recorder);
            if let Some(persistence) = route_persistence {
                persistence.finish().await?;
            }
            Ok(WorkflowRunReport {
                status,
                stages: reports,
            })
        }
        .await;
        // Never drop a Stage future during registration or an ending await.
        // The run token stops admission; each activation owns its cleanup.
        if result.is_err() {
            run_cancel.cancel();
            while running.next().await.is_some() {}
        }
        if !matches!(&result, Ok(report) if report.status == WorkflowStatus::Completed)
            && let Some(supervisor) = context.resident_supervisor.as_ref()
        {
            let (root, _) = engine.session_lineage(lead).await?;
            let endings = futures::future::join_all(
                registered
                    .iter()
                    .map(|actor| supervisor.end_member(root, &actor.handle)),
            )
            .await;
            for ending in endings {
                ending?;
            }
        }
        result
    }
}

/// Map executor Stage state to its durable protocol counterpart.
const fn protocol_stage_status(status: StageStatus) -> WorkflowStageStatus {
    match status {
        StageStatus::Pending => WorkflowStageStatus::Pending,
        StageStatus::Done => WorkflowStageStatus::Completed,
        StageStatus::Failed => WorkflowStageStatus::Failed,
        StageStatus::Cancelled => WorkflowStageStatus::Cancelled,
        StageStatus::Skipped => WorkflowStageStatus::Skipped,
    }
}

fn worst_case_admission_units(stages: &[WorkflowStage]) -> u64 {
    stages.iter().fold(0_u64, |total, stage| {
        let units = match stage.verify() {
            Some(verify) => u64::from(verify.max_iterations()).saturating_mul(2),
            None => 1,
        };
        total.saturating_add(units)
    })
}

/// Check the compiled worst-case activation count before child or mail effects.
/// Registration and revival still enforce capacity in their own transactions.
async fn preflight_workflow_capacity(
    engine: &SessionEngine,
    lead: SessionId,
    workflow: &str,
    wanted: u64,
) -> Result<(), WorkflowError> {
    let Some(governor) = engine.governor().cloned() else {
        return Ok(());
    };
    let (root, depth) = engine.session_lineage(lead).await?;
    if depth.saturating_add(1) > governor.max_depth() {
        return Err(WorkflowError::MaxDepth);
    }
    let limit = governor.limits().per_run_budget;
    if wanted > limit {
        return Err(WorkflowError::Invalid {
            workflow: workflow.to_string(),
            detail: format!(
                "worst-case {wanted} Stage activations exceed the per-run budget ({limit})"
            ),
        });
    }
    let live = engine
        .store()
        .live_member_count(root)
        .await
        .map_err(CoreError::from)?;
    if wanted > limit.saturating_sub(live) {
        return Err(CoreError::from(hya_store::StoreError::CapacityExceeded {
            root,
            live,
            requested: wanted,
            limit,
        })
        .into());
    }
    Ok(())
}

fn validate_resolved_semantics(
    workflow: &str,
    stages: &[WorkflowStage],
    resolved: &[ResolvedAgent],
    ctx: &WorkflowRunContext,
) -> Result<(), WorkflowError> {
    let mut actor_routes: BTreeMap<&str, Option<&WorkflowModelRoute>> = BTreeMap::new();
    for (stage, worker) in stages.iter().zip(resolved) {
        if let Some(actor) = stage.actor() {
            if let Some(existing) = actor_routes.get(actor) {
                if *existing != worker.route.as_ref() {
                    return Err(WorkflowError::Invalid {
                        workflow: workflow.to_string(),
                        detail: format!(
                            "Stages sharing actor `{actor}` must use an identical effective worker route"
                        ),
                    });
                }
            } else {
                actor_routes.insert(actor, worker.route.as_ref());
            }
        }
        // Every Agent is a resident actor (0.41.0): a Stage's `actor` key,
        // not the Agent definition, selects a persistent actor Session; a
        // Stage without one runs a fresh one-shot member Session.
        if let Some(actor) = stage.actor()
            && ctx.resident_supervisor.is_none()
        {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!(
                    "Stage `{}` requires a resident supervisor for actor `{actor}`",
                    stage.id()
                ),
            });
        }
        if stage.actor().is_some() && stage.mode() == StageMode::Loop {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!("Stage `{}` cannot combine actor and loop modes", stage.id()),
            });
        }
    }
    Ok(())
}

fn pending_report(stage: &WorkflowStage) -> StageReport {
    StageReport {
        stage: stage.id().to_string(),
        agent: stage.agent().to_string(),
        status: StageStatus::Pending,
        session: None,
        output: String::new(),
    }
}

fn report_evidence(report: &StageReport) -> Option<StageEvidence<'_>> {
    let status = match report.status {
        StageStatus::Pending => return None,
        StageStatus::Done => StageEvidenceStatus::Done,
        StageStatus::Failed => StageEvidenceStatus::Failed,
        StageStatus::Cancelled => StageEvidenceStatus::Cancelled,
        StageStatus::Skipped => StageEvidenceStatus::Skipped,
    };
    Some(StageEvidence::new(status, &report.output))
}

fn mark_pending(reports: &mut [StageReport], status: StageStatus) {
    for report in reports {
        if report.status == StageStatus::Pending {
            report.status = status;
        }
    }
}

/// The controller accepts both at-rest settlement and a terminal report.
async fn activate_resident_stage(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    supervisor: Arc<ResidentSupervisor>,
    stage: &WorkflowStage,
    activation: ResidentActivation,
) -> Result<StageReport, WorkflowError> {
    let ResidentActivation {
        actor,
        directive,
        system_context,
        cancel,
        actor_claim,
        route,
    } = activation;
    let result = activate_actor(
        &engine,
        &supervisor,
        lead,
        &actor,
        directive,
        system_context,
        route,
        actor_claim.as_ref(),
        &cancel,
    )
    .await;
    let (status, output) = match result {
        Ok(text) => (StageStatus::Done, text),
        Err(CoreError::Cancelled) => (
            StageStatus::Cancelled,
            "Workflow activation cancelled".into(),
        ),
        Err(error @ CoreError::Store(hya_store::StoreError::CapacityExceeded { .. })) => {
            return Err(error.into());
        }
        Err(error) => (StageStatus::Failed, error.to_string()),
    };
    Ok(StageReport {
        stage: stage.id().to_string(),
        agent: stage.agent().to_string(),
        status,
        session: Some(actor.session.to_string()),
        output: clamp(output),
    })
}

/// Settlement retains an episode; a report has already released its claim.
#[allow(clippy::too_many_arguments)]
async fn activate_actor(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    parent: SessionId,
    actor: &ResidentActor,
    directive: String,
    guidance: Arc<str>,
    route: Option<WorkflowTurnRoute>,
    parent_claim: Option<&ActorClaim>,
    cancel: &CancellationToken,
) -> Result<String, CoreError> {
    if cancel.is_cancelled() {
        return Err(CoreError::Cancelled);
    }
    let mut waiter = supervisor
        .activate(
            parent,
            actor.session,
            directive,
            guidance,
            route,
            parent_claim,
        )
        .await?;
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let (root, _) = engine.session_lineage(parent).await?;
            supervisor.end_member(root, &actor.handle).await?;
            waiter.wait().await?;
            return Err(CoreError::Cancelled);
        }
        result = waiter.wait() => result?,
    };
    match result {
        ActivationResult::Settled { final_text } => Ok(final_text),
        ActivationResult::Reported {
            outcome: ReportOutcome::Done,
            text,
        } => Ok(text),
        ActivationResult::Reported { text, .. } => Err(CoreError::Invalid(text)),
        ActivationResult::Ended { note, .. } => Err(CoreError::Invalid(note)),
    }
}

fn clamp(mut text: String) -> String {
    if text.len() <= MAX_PREDECESSOR_OUTPUT_BYTES {
        return text;
    }
    let mut end = MAX_PREDECESSOR_OUTPUT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

// ---------------------------------------------------------------------------
// Loop stages
// ---------------------------------------------------------------------------

/// Drive one loop stage through the shared [`IterationDriver`]: the stored
/// first-round output counts as iteration one, further rounds resume the same
/// child session, and an independent verifier member judges each transcript.
#[allow(clippy::too_many_arguments)]
async fn drive_loop_stage(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    workflow_name: &str,
    ctx: &WorkflowRunContext,
    stage: &WorkflowStage,
    verify: &VerifySpec,
    directive: &str,
    system_context: &str,
    verification_condition: &str,
    worker: ResidentActor,
    resolved_worker: &ResolvedAgent,
    resolved_verifier: ResolvedAgent,
    first_report: StageReport,
    cancel: CancellationToken,
    actor_claim: Option<ActorClaim>,
    durable_run: Option<WorkflowRunId>,
    route_recorder: Option<WorkflowRouteRecorder>,
) -> Result<StageReport, WorkflowError> {
    let executor =
        LoopWorkerExecutor {
            engine: engine.clone(),
            lead,
            supervisor: ctx.resident_supervisor.as_ref().cloned().ok_or_else(|| {
                CoreError::Invalid("Workflow requires a resident supervisor".into())
            })?,
            worker,
            route: resolved_worker.route.clone(),
            cancel: cancel.clone(),
            first: tokio::sync::Mutex::new(Some(first_report.output.clone())),
            latest_output: tokio::sync::Mutex::new(first_report.output.clone()),
            guidance: Arc::from(system_context.to_string()),
            actor_claim,
            durable_run,
            route_recorder: route_recorder.clone(),
            stage_id: stage.id().to_string(),
            next_iteration: tokio::sync::Mutex::new(1),
        };

    let gate = VerifierGate {
        engine,
        lead,
        ctx: ctx.clone(),
        supervisor: executor.supervisor.clone(),
        verifier: resolved_verifier,
        until: verification_condition.to_string(),
        label: format!("Workflow {workflow_name} / {}", stage.id()),
        cancel: cancel.clone(),
        guidance: Arc::from(system_context.to_string()),
        actor_claim,
        durable_run,
        stage_id: stage.id().to_string(),
        route_recorder,
        next_iteration: tokio::sync::Mutex::new(0),
    };
    let caps = SafetyCaps {
        max_iterations: verify.max_iterations(),
        ..SafetyCaps::default()
    };
    let outcome = IterationDriver::new(caps)
        .run(
            &executor,
            &gate,
            format!("{directive}\nContinue working toward the verified condition."),
            // Activations own cancellation and cleanup. The driver's select
            // would drop a judgment during registration or its ending await.
            CancellationToken::new(),
        )
        .await?;

    let mut report = first_report;
    report.output = match outcome {
        crate::completion::RunOutcome::Achieved { reason, .. } => {
            format!("{}\n\n[verified: {reason}]", clamp(executor.latest().await))
        }
        crate::completion::RunOutcome::BudgetLimited { iterations, .. } => {
            format!(
                "{}\n\n[loop budget limited after {iterations} iterations without verification]",
                clamp(executor.latest().await)
            )
        }
        crate::completion::RunOutcome::Stalled { iterations } => {
            format!(
                "{}\n\n[loop stalled after {iterations} identical iterations]",
                clamp(executor.latest().await)
            )
        }
        crate::completion::RunOutcome::Capped { iterations, .. } => {
            format!(
                "{}\n\n[loop capped after {iterations} iterations without verification]",
                clamp(executor.latest().await)
            )
        }
        crate::completion::RunOutcome::InvalidCondition { reason, .. } => {
            return Err(CoreError::Invalid(reason).into());
        }
        crate::completion::RunOutcome::Cancelled => {
            report.status = StageStatus::Cancelled;
            format!("{}\n\n[loop cancelled]", clamp(executor.latest().await))
        }
    };
    Ok(report)
}

/// Worker-side [`IterationExecutor`]-built gate wrapper; see [`VerifierGate`].
struct LoopWorkerExecutor {
    engine: Arc<SessionEngine>,
    lead: SessionId,
    supervisor: Arc<ResidentSupervisor>,
    worker: ResidentActor,
    route: Option<WorkflowModelRoute>,
    cancel: CancellationToken,
    first: tokio::sync::Mutex<Option<String>>,
    latest_output: tokio::sync::Mutex<String>,
    guidance: Arc<str>,
    actor_claim: Option<ActorClaim>,
    durable_run: Option<WorkflowRunId>,
    route_recorder: Option<WorkflowRouteRecorder>,
    stage_id: String,
    next_iteration: tokio::sync::Mutex<u32>,
}

impl LoopWorkerExecutor {
    async fn latest(&self) -> String {
        self.latest_output.lock().await.clone()
    }
}

#[async_trait]
impl IterationExecutor for LoopWorkerExecutor {
    async fn run_iteration(
        &self,
        directive: &str,
        _cancel: &CancellationToken,
    ) -> Result<String, CoreError> {
        if self.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        if let Some(first) = self.first.lock().await.take() {
            return Ok(first);
        }
        let member = self.worker.member;
        let mut next = self.next_iteration.lock().await;
        let iteration = *next;
        *next = next.saturating_add(1);
        drop(next);
        if let Some(run) = self.durable_run {
            self.engine
                .record_workflow_event_for_actor(
                    self.actor_claim.as_ref(),
                    self.lead,
                    Event::WorkflowStageMemberLinked {
                        session: self.lead,
                        run,
                        stage: self.stage_id.clone(),
                        member,
                        role: WorkflowMemberRole::Worker,
                        iteration,
                    },
                )
                .await
                .map_err(|error| CoreError::Invalid(error.to_string()))?;
        }
        let route = self.route.clone().map(|route| {
            WorkflowTurnRoute::new(WorkflowTurnRouteSpec {
                route,
                session: self.lead,
                run: self.durable_run,
                stage: self.stage_id.clone(),
                member,
                role: WorkflowMemberRole::Worker,
                iteration,
                recorder: self.route_recorder.clone(),
            })
        });
        let text = activate_actor(
            &self.engine,
            &self.supervisor,
            self.lead,
            &self.worker,
            directive.to_string(),
            self.guidance.clone(),
            route,
            self.actor_claim.as_ref(),
            &self.cancel,
        )
        .await?;
        *self.latest_output.lock().await = text.clone();
        Ok(text)
    }
}

/// Independent stop authority: spawns a fresh verifier member per judgment and
/// parses its strict JSON verdict tolerantly (malformed ⇒ not met).
struct VerifierGate {
    engine: Arc<SessionEngine>,
    lead: SessionId,
    ctx: WorkflowRunContext,
    supervisor: Arc<ResidentSupervisor>,
    verifier: ResolvedAgent,
    until: String,
    label: String,
    cancel: CancellationToken,
    guidance: Arc<str>,
    actor_claim: Option<ActorClaim>,
    durable_run: Option<WorkflowRunId>,
    route_recorder: Option<WorkflowRouteRecorder>,
    stage_id: String,
    next_iteration: tokio::sync::Mutex<u32>,
}

#[async_trait]
impl IterationGate for VerifierGate {
    async fn judge(&self, transcript: &str) -> Result<GateOutcome, CoreError> {
        let directive = format!(
            "You are an independent verifier with no stake in the work. Judge ONLY \
             whether the condition below is satisfied by the latest worker output.\n\
             CONDITION: {}\n\nLATEST WORKER OUTPUT:\n{}\n\nRespond with ONLY a JSON \
             object: {{\"met\": true|false, \"reason\": \"...\"}}. If you cannot see \
             evidence the condition holds, answer met=false.",
            self.until,
            render_transcript_bounded(transcript)
        );
        if self.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let (session, handle, member) = self
            .supervisor
            .spawn_resident_parked(
                self.lead,
                self.verifier.spec.clone(),
                (
                    self.ctx.binding.clone(),
                    self.verifier.agents.clone(),
                    self.verifier.resources.clone(),
                    self.verifier.sidecar_factory.clone(),
                ),
                format!("{} verify", self.label),
                self.actor_claim.as_ref(),
                Some(self.guidance.clone()),
            )
            .await?;
        let actor = ResidentActor {
            session,
            handle,
            member,
        };
        let result = async {
            let mut next = self.next_iteration.lock().await;
            let iteration = *next;
            *next = next.saturating_add(1);
            drop(next);
            if let Some(run) = self.durable_run {
                self.engine
                    .record_workflow_event_for_actor(
                        self.actor_claim.as_ref(),
                        self.lead,
                        Event::WorkflowStageMemberLinked {
                            session: self.lead,
                            run,
                            stage: self.stage_id.clone(),
                            member,
                            role: WorkflowMemberRole::Verifier,
                            iteration,
                        },
                    )
                    .await?;
            }
            let route = self.verifier.route.clone().map(|route| {
                WorkflowTurnRoute::new(WorkflowTurnRouteSpec {
                    route,
                    session: self.lead,
                    run: self.durable_run,
                    stage: self.stage_id.clone(),
                    member,
                    role: WorkflowMemberRole::Verifier,
                    iteration,
                    recorder: self.route_recorder.clone(),
                })
            });
            activate_actor(
                &self.engine,
                &self.supervisor,
                self.lead,
                &actor,
                directive,
                self.guidance.clone(),
                route,
                self.actor_claim.as_ref(),
                &self.cancel,
            )
            .await
        }
        .await;
        // A judgment never retains a capacity claim, including link/route errors
        // and cancellation. Reported episodes are already ended here.
        let (root, _) = self.engine.session_lineage(self.lead).await?;
        self.supervisor.end_member(root, &actor.handle).await?;
        let verdict_text = result?;
        let verdict = parse_verdict(&verdict_text);
        if verdict.met {
            Ok(GateOutcome::Stop {
                reason: verdict.reason,
            })
        } else {
            Ok(GateOutcome::Continue {
                directive: format!(
                    "{}\n\nThe verifier reports the condition is not yet met: {}\n\
                     Continue working toward it.",
                    self.until, verdict.reason
                ),
            })
        }
    }
}

/// Clamp verifier-bound transcripts so judgments stay bounded.
fn render_transcript_bounded(transcript: &str) -> String {
    clamp(transcript.to_string())
}

struct Verdict {
    met: bool,
    reason: String,
}

/// Tolerant strict-JSON verdict parsing: any malformed output counts as
/// "not met" toward the cap instead of failing the whole run.
fn parse_verdict(text: &str) -> Verdict {
    let trimmed = text.trim();
    let Some(start) = trimmed.find('{') else {
        return Verdict {
            met: false,
            reason: "verifier returned malformed output".to_string(),
        };
    };
    let Some(end) = trimmed.rfind('}') else {
        return Verdict {
            met: false,
            reason: "verifier returned malformed output".to_string(),
        };
    };
    match serde_json::from_str::<serde_json::Value>(&trimmed[start..=end]) {
        Ok(value) => Verdict {
            met: value.get("met").and_then(|v| v.as_bool()).unwrap_or(false),
            reason: value
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("no reason given")
                .to_string(),
        },
        Err(_) => Verdict {
            met: false,
            reason: "verifier returned malformed output".to_string(),
        },
    }
}
