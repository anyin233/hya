//! Workflow execution over the governed subagent team primitives.
//!
//! Each topological level is executed as ONE batch through
//! [`pre_admit_team`](crate::subagent::pre_admit_team) /
//! [`run_pre_admitted_team`](crate::subagent::run_pre_admitted_team), so the
//! [`SubagentGovernor`](crate::SubagentGovernor) still bounds depth, per-run
//! spawn budget, and streaming concurrency for user-authored DAGs exactly as it
//! does for model-decided `task` batches. Loop stages iterate through
//! [`IterationDriver`](crate::completion::IterationDriver) with an independent
//! verifier member — never a second loop implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::SpawnLifecycle;
use hya_proto::{
    ActorClaim, Event, MailEndpoint, MailKind, MemberId, ModelRef, RosterStatus, SessionId,
    WorkflowMemberRole, WorkflowRunId, WorkflowStageStatus,
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
use crate::resident::ResidentSupervisor;
use crate::sidecar::BoundSidecarFactory;
use crate::subagent::{
    MemberEvidence, MemberSpec, MemberStatus, run_pre_admitted_team_with_workflow,
};

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
    /// Fail-fast prevented the Stage from starting.
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
    /// Every level of the DAG ran per its declared failure policy.
    Completed,
    /// Fail-fast aborted the run because a level had a failed member.
    Failed,
    /// Cancellation tripped before some levels ran.
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
    lifecycle: SpawnLifecycle,
    route: Option<WorkflowModelRoute>,
}

#[derive(Clone)]
struct ResidentActor {
    session: SessionId,
    handle: String,
    member: MemberId,
}

struct PreparedActivation {
    index: usize,
    directive: String,
    system_context: Arc<str>,
}
struct ResidentActivation {
    actor: ResidentActor,
    prepared: PreparedActivation,
    cancel: CancellationToken,
    actor_claim: Option<ActorClaim>,
    route: Option<WorkflowTurnRoute>,
}

struct WorkflowBudgetReservation {
    governor: crate::orchestrator::SubagentGovernor,
    root: SessionId,
    units: u64,
}

impl Drop for WorkflowBudgetReservation {
    fn drop(&mut self) {
        self.governor.refund_reserved(self.root, self.units);
    }
}

/// Fully preflighted and budget-reserved Workflow execution.
///
/// Dropping this value before [`Self::execute`] refunds the scoped reservation
/// without creating a child Session or sending mail.
pub struct PreparedWorkflowRun {
    engine: Arc<SessionEngine>,
    lead: SessionId,
    workflow: CompiledWorkflow,
    context: WorkflowRunContext,
    resolved: Vec<ResolvedAgent>,
    verifiers: Vec<Option<ResolvedAgent>>,
    _budget: Option<WorkflowBudgetReservation>,
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
        lifecycle: definition.spawn_lifecycle,
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
    validate_resolved_semantics(name, plan.stages(), &resolved, &verifiers, &context)?;
    let budget = reserve_workflow_budget(&engine, lead, name, wanted).await?;
    Ok(PreparedWorkflowRun {
        engine,
        lead,
        workflow: workflow.clone(),
        context,
        resolved,
        verifiers,
        _budget: budget,
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

/// Execute the already-validated graph and consume its budget reservation.
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
    /// Execute the already-validated graph and consume its budget reservation.
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
            _budget,
            durable_run,
            actor_claim,
        } = self;
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
        for level in plan.levels() {
            if cancel.is_cancelled() {
                mark_pending(&mut reports, StageStatus::Cancelled);
                return Ok(WorkflowRunReport {
                    status: WorkflowStatus::Cancelled,
                    stages: reports,
                });
            }

            let stage_evidence = reports.iter().map(report_evidence).collect::<Vec<_>>();
            let mut activations = Vec::with_capacity(level.stage_indices().len());
            for &index in level.stage_indices() {
                let rendered = workflow.render_stage(index, &context.inputs, &stage_evidence)?;
                activations.push(PreparedActivation {
                    index,
                    directive: rendered.directive().to_string(),
                    system_context: Arc::from(rendered.system_context().to_string()),
                });
            }
            if let Some(run) = durable_run {
                for &index in level.stage_indices() {
                    let stage = &plan.stages()[index];
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
            }

            let mut transient_members = Vec::new();
            let mut transient_specs = Vec::new();
            let mut transient_routes: BTreeMap<MemberId, WorkflowTurnRoute> = BTreeMap::new();
            let mut resident_activations: Vec<ResidentActivation> = Vec::new();
            for activation in activations {
                let stage = &plan.stages()[activation.index];
                let resolved_stage = resolved[activation.index].clone();
                if let Some(actor_key) = stage.actor() {
                    let Some(supervisor) = context.resident_supervisor.as_ref() else {
                        return Err(WorkflowError::Invalid {
                            workflow: name.to_string(),
                            detail: "resident supervisor disappeared after preflight".to_string(),
                        });
                    };
                    let actor = if let Some(existing) = actors.get(actor_key).cloned() {
                        existing
                    } else {
                        let spawn_result = supervisor
                            .spawn_resident_parked(
                                lead,
                                resolved_stage.spec.clone(),
                                (
                                    context.binding.clone(),
                                    resolved_stage.agents.clone(),
                                    resolved_stage.resources.clone(),
                                    resolved_stage.sidecar_factory.clone(),
                                ),
                                format!("Workflow {name} resident actor `{actor_key}`"),
                                actor_claim.as_ref(),
                                Some(activation.system_context.clone()),
                            )
                            .await;
                        let (session, handle, member) = spawn_result?;
                        let actor = ResidentActor {
                            session,
                            handle,
                            member,
                        };
                        actors.insert(actor_key.to_string(), actor.clone());
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
                    let route = resolved_stage.route.map(|route| {
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
                    resident_activations.push(ResidentActivation {
                        actor,
                        prepared: activation,
                        cancel: cancel.child_token(),
                        actor_claim,
                        route,
                    });
                } else {
                    let member = MemberId::new();
                    transient_members.push((activation.index, member));
                    transient_specs.push(MemberSpec {
                        id: member,
                        agent: resolved_stage.spec,
                        binding: context.binding.clone(),
                        agents: resolved_stage.agents,
                        resources: Some(resolved_stage.resources),
                        guidance: Some(activation.system_context),
                        directive: activation.directive,
                        tool_call: None,
                        description: format!("Workflow {name} / {}", stage.id()),
                        session: None,
                        sidecar_factory: resolved_stage.sidecar_factory,
                    });
                    if let Some(route) = resolved_stage.route {
                        transient_routes.insert(
                            member,
                            WorkflowTurnRoute::new(WorkflowTurnRouteSpec {
                                route,
                                session: lead,
                                run: durable_run,
                                stage: stage.id().to_string(),
                                member,
                                role: WorkflowMemberRole::Worker,
                                iteration: 0,
                                recorder: route_recorder.clone(),
                            }),
                        );
                    }
                }
            }
            if let Some(run) = durable_run {
                for &(index, member) in &transient_members {
                    engine
                        .record_workflow_event_for_actor(
                            actor_claim.as_ref(),
                            lead,
                            Event::WorkflowStageMemberLinked {
                                session: lead,
                                run,
                                stage: plan.stages()[index].id().to_string(),
                                member,
                                role: WorkflowMemberRole::Worker,
                                iteration: 0,
                            },
                        )
                        .await?;
                }
            }

            let transient_engine = engine.clone();
            let transient_cancel = cancel.clone();
            let transient_run = async move {
                if transient_specs.is_empty() {
                    Vec::new()
                } else {
                    let routed_specs = transient_specs
                        .into_iter()
                        .map(|spec| {
                            let route = transient_routes.remove(&spec.id);
                            (spec, route)
                        })
                        .collect();
                    run_pre_admitted_team_with_workflow(
                        transient_engine,
                        lead,
                        routed_specs,
                        transient_cancel.child_token(),
                        actor_claim,
                    )
                    .await
                }
            };
            let resident_run =
                futures::future::join_all(resident_activations.into_iter().map(|activation| {
                    let engine = engine.clone();
                    let supervisor = context.resident_supervisor.as_ref().cloned();
                    let stage = plan.stages()[activation.prepared.index].clone();
                    async move {
                        let Some(supervisor) = supervisor else {
                            return Err(WorkflowError::Invalid {
                                workflow: name.to_string(),
                                detail: "resident supervisor disappeared after spawn".to_string(),
                            });
                        };
                        let index = activation.prepared.index;
                        let report =
                            activate_resident_stage(engine, lead, supervisor, &stage, activation)
                                .await?;
                        Ok::<_, WorkflowError>((index, report))
                    }
                }));
            let (evidence, resident_results) = tokio::join!(transient_run, resident_run);
            let cancelled = cancel.is_cancelled();
            for (&(index, _), evidence) in transient_members.iter().zip(evidence.iter()) {
                let report =
                    stage_report(&engine, &plan.stages()[index], evidence, cancelled).await?;
                reports[index] = report;
            }
            for result in resident_results {
                let (index, report) = result?;
                reports[index] = report;
            }

            for &index in level.stage_indices() {
                let stage = &plan.stages()[index];
                let Some(verify) = stage.verify() else {
                    continue;
                };
                if reports[index].status != StageStatus::Done {
                    continue;
                }
                let Some(verifier) = verifiers[index].clone() else {
                    continue;
                };
                let stage_evidence = reports.iter().map(report_evidence).collect::<Vec<_>>();
                let rendered = workflow.render_stage(index, &context.inputs, &stage_evidence)?;
                let verification_condition = rendered
                    .verification_condition()
                    .unwrap_or_else(|| verify.until());
                let mut report = reports[index].clone();
                match drive_loop_stage(
                    engine.clone(),
                    lead,
                    name,
                    &context,
                    stage,
                    verify,
                    rendered.directive(),
                    rendered.system_context(),
                    verification_condition,
                    &resolved[index],
                    verifier.clone(),
                    report.clone(),
                    cancel.clone(),
                    actor_claim,
                    durable_run,
                    route_recorder.clone(),
                )
                .await
                {
                    Ok(completed) => reports[index] = completed,
                    Err(error) => {
                        report.status = if cancel.is_cancelled() {
                            StageStatus::Cancelled
                        } else {
                            StageStatus::Failed
                        };
                        report.output = clamp(format!("loop Stage failed: {error}"));
                        reports[index] = report;
                    }
                }
            }
            if let Some(run) = durable_run {
                for &index in level.stage_indices() {
                    let report = &reports[index];
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
            }

            if cancel.is_cancelled() {
                mark_pending(&mut reports, StageStatus::Cancelled);
                return Ok(WorkflowRunReport {
                    status: WorkflowStatus::Cancelled,
                    stages: reports,
                });
            }
            if workflow.definition().on_failure() == FailurePolicy::FailFast
                && level_failed(&reports, level.stage_indices())
            {
                mark_pending(&mut reports, StageStatus::Skipped);
                return Ok(WorkflowRunReport {
                    status: WorkflowStatus::Failed,
                    stages: reports,
                });
            }
        }

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

/// Reserve the compiled worst-case activation count before the first child or
/// mail effect; the returned guard refunds the scoped reservation on every exit.
async fn reserve_workflow_budget(
    engine: &SessionEngine,
    lead: SessionId,
    workflow: &str,
    wanted: u64,
) -> Result<Option<WorkflowBudgetReservation>, WorkflowError> {
    let Some(governor) = engine.governor().cloned() else {
        return Ok(None);
    };
    let (root, depth) = engine.session_lineage(lead).await?;
    if depth.saturating_add(1) > governor.max_depth() {
        return Err(WorkflowError::Admission(
            crate::subagent::TeamAdmissionError::MaxDepth,
        ));
    }
    let remaining = governor.remaining_budget(root);
    if wanted > remaining {
        return Err(WorkflowError::Invalid {
            workflow: workflow.to_string(),
            detail: format!(
                "worst-case {wanted} Stage activations exceed the per-run budget (remaining {remaining})"
            ),
        });
    }
    if !governor.try_reserve_exact(root, wanted) {
        return Err(WorkflowError::Admission(
            crate::subagent::TeamAdmissionError::BudgetExhausted,
        ));
    }
    Ok(Some(WorkflowBudgetReservation {
        governor,
        root,
        units: wanted,
    }))
}

fn validate_resolved_semantics(
    workflow: &str,
    stages: &[WorkflowStage],
    resolved: &[ResolvedAgent],
    verifiers: &[Option<ResolvedAgent>],
    ctx: &WorkflowRunContext,
) -> Result<(), WorkflowError> {
    let mut actor_routes: BTreeMap<&str, Option<&WorkflowModelRoute>> = BTreeMap::new();
    for ((stage, worker), verifier) in stages.iter().zip(resolved).zip(verifiers) {
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
        match (stage.actor(), worker.lifecycle) {
            (Some(_), SpawnLifecycle::Resident) => {
                if ctx.resident_supervisor.is_none() {
                    return Err(WorkflowError::Invalid {
                        workflow: workflow.to_string(),
                        detail: format!(
                            "Stage `{}` requires a resident supervisor for actor `{}`",
                            stage.id(),
                            stage.actor().unwrap_or_default()
                        ),
                    });
                }
            }
            (Some(actor), SpawnLifecycle::Transient) => {
                return Err(WorkflowError::Invalid {
                    workflow: workflow.to_string(),
                    detail: format!(
                        "Stage `{}` actor `{actor}` targets transient Agent `{}`",
                        stage.id(),
                        stage.agent()
                    ),
                });
            }
            (None, SpawnLifecycle::Resident) => {
                return Err(WorkflowError::Invalid {
                    workflow: workflow.to_string(),
                    detail: format!(
                        "Stage `{}` targets resident Agent `{}` without an actor key",
                        stage.id(),
                        stage.agent()
                    ),
                });
            }
            (None, SpawnLifecycle::Transient) => {}
        }
        if stage.actor().is_some() && stage.mode() == StageMode::Loop {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!("Stage `{}` cannot combine actor and loop modes", stage.id()),
            });
        }
        if verifier
            .as_ref()
            .is_some_and(|agent| agent.lifecycle == SpawnLifecycle::Resident)
        {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!("Stage `{}` verifier Agent must be transient", stage.id()),
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

fn level_failed(reports: &[StageReport], level: &[usize]) -> bool {
    level
        .iter()
        .any(|&index| reports[index].status == StageStatus::Failed)
}

async fn stage_report(
    engine: &Arc<SessionEngine>,
    stage: &WorkflowStage,
    evidence: &MemberEvidence,
    cancelled: bool,
) -> Result<StageReport, WorkflowError> {
    let base = StageReport {
        stage: stage.id().to_string(),
        agent: stage.agent().to_string(),
        status: if cancelled {
            StageStatus::Cancelled
        } else {
            StageStatus::Failed
        },
        session: None,
        output: String::new(),
    };
    if evidence.status != MemberStatus::Done {
        return Ok(StageReport {
            output: clamp(evidence.summary.clone()),
            ..base
        });
    }
    let Ok(child) = evidence.session.parse::<SessionId>() else {
        return Ok(base);
    };
    let projection = engine.read_projection(child).await?;
    Ok(StageReport {
        status: StageStatus::Done,
        session: Some(evidence.session.clone()),
        output: final_assistant_text(&projection),
        ..base
    })
}

/// Last assistant text, UTF-8 safely bounded for downstream evidence.
fn final_assistant_text(projection: &hya_proto::Projection) -> String {
    for message in projection.session.messages.iter().rev() {
        if !matches!(message.role, hya_proto::Role::Assistant) {
            continue;
        }
        let mut text = String::new();
        for part in &message.parts {
            if let hya_proto::PartProjection::Text { text: t, .. } = part {
                text.push_str(t);
            }
        }
        return clamp(text);
    }
    String::new()
}

/// Deliver one resident Stage as durable mail and wait until Projection proves
/// that exact inbox boundary reached idle or failed.
async fn activate_resident_stage(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    supervisor: Arc<ResidentSupervisor>,
    stage: &WorkflowStage,
    activation: ResidentActivation,
) -> Result<StageReport, WorkflowError> {
    let ResidentActivation {
        actor,
        prepared,
        cancel,
        actor_claim,
        route,
    } = activation;
    let PreparedActivation {
        directive,
        system_context,
        ..
    } = prepared;
    supervisor
        .set_resident_workflow_activation(actor.session, system_context, route)
        .await?;
    let mut events = engine.bus().subscribe();
    engine
        .mail_send_for_actor(
            lead,
            MailEndpoint::Handle(actor.handle.clone()),
            MailKind::Message,
            directive,
            actor_claim.as_ref(),
        )
        .await?;
    let (root, _) = engine.session_lineage(lead).await?;
    let boundary = engine
        .read_projection(root)
        .await?
        .team
        .inboxes
        .get(&actor.handle)
        .map(Vec::len)
        .and_then(|length| u64::try_from(length).ok())
        .unwrap_or(u64::MAX);
    let mut stop_requested = false;

    loop {
        let projection = engine.read_projection(root).await?;
        if let Some(entry) = projection.team.roster.get(&actor.handle)
            && entry.resident_cursor >= boundary
            && entry.resident_work.is_none()
        {
            match entry.status {
                RosterStatus::Idle => {
                    let output =
                        final_assistant_text(&engine.read_projection(actor.session).await?);
                    return Ok(StageReport {
                        stage: stage.id().to_string(),
                        agent: stage.agent().to_string(),
                        status: StageStatus::Done,
                        session: Some(actor.session.to_string()),
                        output,
                    });
                }
                RosterStatus::Failed | RosterStatus::Done => {
                    return Ok(StageReport {
                        stage: stage.id().to_string(),
                        agent: stage.agent().to_string(),
                        status: if stop_requested {
                            StageStatus::Cancelled
                        } else {
                            StageStatus::Failed
                        },
                        session: Some(actor.session.to_string()),
                        output: clamp(
                            entry
                                .current_task
                                .clone()
                                .unwrap_or_else(|| "resident actor failed".to_string()),
                        ),
                    });
                }
                RosterStatus::Busy => {}
            }
        }
        let event = if stop_requested {
            Some(events.recv().await)
        } else {
            tokio::select! {
                _ = cancel.cancelled() => None,
                event = events.recv() => Some(event),
            }
        };
        let Some(event) = event else {
            supervisor.stop_resident(root, &actor.handle).await?;
            stop_requested = true;
            continue;
        };
        match event {
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return Err(WorkflowError::Engine(CoreError::Invalid(
                    "event bus closed before resident Stage settled".to_string(),
                )));
            }
        }
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
    resolved_worker: &ResolvedAgent,
    resolved_verifier: ResolvedAgent,
    first_report: StageReport,
    cancel: CancellationToken,
    actor_claim: Option<ActorClaim>,
    durable_run: Option<WorkflowRunId>,
    route_recorder: Option<WorkflowRouteRecorder>,
) -> Result<StageReport, WorkflowError> {
    let worker_session = first_report
        .session
        .as_deref()
        .and_then(|s| s.parse::<SessionId>().ok());

    let executor = LoopWorkerExecutor {
        engine: engine.clone(),
        lead,
        ctx: ctx.clone(),
        worker: resolved_worker.clone(),
        session: tokio::sync::Mutex::new(worker_session),
        first: tokio::sync::Mutex::new(Some(first_report.output.clone())),
        latest_output: tokio::sync::Mutex::new(first_report.output.clone()),
        label: format!("Workflow {workflow_name} / {}", stage.id()),
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
        verifier: resolved_verifier,
        until: verification_condition.to_string(),
        label: executor.label.clone(),
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
            cancel.child_token(),
        )
        .await?;

    let mut report = first_report;
    report.output = match outcome {
        crate::completion::RunOutcome::Achieved { reason, .. } => {
            format!("{}\n\n[verified: {reason}]", clamp(executor.latest().await))
        }
        crate::completion::RunOutcome::Capped { iterations, .. } => {
            format!(
                "{}\n\n[loop capped after {iterations} iterations without verification]",
                clamp(executor.latest().await)
            )
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
    ctx: WorkflowRunContext,
    worker: ResolvedAgent,
    session: tokio::sync::Mutex<Option<SessionId>>,
    first: tokio::sync::Mutex<Option<String>>,
    latest_output: tokio::sync::Mutex<String>,
    label: String,
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
        cancel: &CancellationToken,
    ) -> Result<String, CoreError> {
        if let Some(first) = self.first.lock().await.take() {
            return Ok(first);
        }
        let resumed = *self.session.lock().await;
        let member = MemberId::new();
        let spec = MemberSpec {
            id: member,
            resources: Some(self.worker.resources.clone()),
            agent: self.worker.spec.clone(),
            binding: self.ctx.binding.clone(),
            agents: self.worker.agents.clone(),
            guidance: Some(self.guidance.clone()),
            directive: directive.to_string(),
            tool_call: None,
            description: format!("{} (iteration)", self.label),
            session: resumed,
            sidecar_factory: self.worker.sidecar_factory.clone(),
        };
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
        let route = self.worker.route.clone().map(|route| {
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
        let evidence = run_pre_admitted_team_with_workflow(
            self.engine.clone(),
            self.lead,
            vec![(spec, route)],
            cancel.child_token(),
            self.actor_claim,
        )
        .await;
        let Some(entry) = evidence.first() else {
            return Err(CoreError::Invalid(
                "workflow worker produced no evidence".to_string(),
            ));
        };
        if entry.status != MemberStatus::Done {
            return Err(CoreError::Cancelled);
        }
        if let Ok(session) = entry.session.parse::<SessionId>() {
            *self.session.lock().await = Some(session);
        }
        let projection = self
            .engine
            .read_projection(
                entry
                    .session
                    .parse::<SessionId>()
                    .map_err(|_| CoreError::Invalid("unparsable member session".to_string()))?,
            )
            .await?;
        let text = final_assistant_text(&projection);
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
        let member = MemberId::new();
        let spec = MemberSpec {
            id: member,
            resources: Some(self.verifier.resources.clone()),
            agent: self.verifier.spec.clone(),
            binding: self.ctx.binding.clone(),
            agents: self.verifier.agents.clone(),
            guidance: Some(self.guidance.clone()),
            directive,
            tool_call: None,
            description: format!("{} verify", self.label),
            session: None,
            sidecar_factory: self.verifier.sidecar_factory.clone(),
        };
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
        let evidence = run_pre_admitted_team_with_workflow(
            self.engine.clone(),
            self.lead,
            vec![(spec, route)],
            self.cancel.child_token(),
            self.actor_claim,
        )
        .await;
        let Some(entry) = evidence.first() else {
            return Err(CoreError::Invalid(
                "verifier produced no evidence".to_string(),
            ));
        };
        if entry.status != MemberStatus::Done {
            return Err(CoreError::Cancelled);
        }
        let verdict_text = if let Ok(session) = entry.session.parse::<SessionId>() {
            final_assistant_text(&self.engine.read_projection(session).await?)
        } else {
            entry.summary.clone()
        };
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
