//! Workflow DAG preflight, topological scheduling, loop verification, and durable event emission.
//!
//! All host interactions flow exclusively through `Arc<dyn HostRuntime>`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use hya_proto::{
    ActorClaim, AgentName, ModelRef, SessionId, WorkflowModelAssignment, WorkflowRunId,
};
use hya_runtime_abi::{
    HostError, HostRuntime, ManagedMemberActivateRequest, ManagedMemberActivationStatus,
    ManagedMemberHandle, ManagedMemberSpawnRequest, ModelRouteEntry, ResolvedModelRoute,
    TurnRouteFailureClass, TurnRouteOutcome,
};
use hya_tool::extension::ExtensionEvent;
use hya_tool::{WorkflowMemberRole, WorkflowRouteFailureClass, WorkflowStageStatus};
use hya_workflow::{
    CompiledWorkflow, FailurePolicy, MAX_PREDECESSOR_OUTPUT_BYTES, StageEvidence, StageMode,
    VerifySpec, WorkflowStage,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::error::WorkflowError;
use crate::reducer::extension_event_parts;

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
    /// The activated Stage was cancelled.
    Cancelled,
    /// The Stage never activated (failure policy, predecessor, or cancellation).
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
/// in declaration order.
#[derive(Clone, Debug, Serialize)]
pub struct WorkflowRunReport {
    /// Overall terminal state.
    pub status: WorkflowStatus,
    /// One row per compiled Stage; unstarted Stages are skipped.
    pub stages: Vec<StageReport>,
}

/// Fully preflighted Workflow execution running exclusively over [`HostRuntime`].
pub struct PreparedWorkflowRun {
    runtime: Arc<dyn HostRuntime>,
    lead: SessionId,
    caller: AgentName,
    workflow: CompiledWorkflow,
    inputs: BTreeMap<String, String>,
    worker_routes: Vec<Option<ResolvedModelRoute>>,
    verifier_routes: Vec<Option<ResolvedModelRoute>>,
    durable_run: Option<WorkflowRunId>,
    actor_claim: Option<ActorClaim>,
}

impl PreparedWorkflowRun {
    /// Return the prepared worker route for a Stage, when explicitly assigned.
    #[must_use]
    pub fn worker_route(&self, stage_index: usize) -> Option<&ResolvedModelRoute> {
        self.worker_routes.get(stage_index)?.as_ref()
    }

    /// Return the prepared verifier route for a Stage, when explicitly assigned.
    #[must_use]
    pub fn verifier_route(&self, stage_index: usize) -> Option<&ResolvedModelRoute> {
        self.verifier_routes.get(stage_index)?.as_ref()
    }

    /// Whether any Stage has an explicit worker or verifier route.
    #[must_use]
    pub fn has_explicit_routes(&self) -> bool {
        self.worker_routes.iter().any(Option::is_some)
            || self.verifier_routes.iter().any(Option::is_some)
    }

    /// Execute the already-validated DAG; registrations consume live capacity.
    ///
    /// # Errors
    /// Returns rendering or governed Stage execution failures.
    pub async fn execute(
        self,
        cancel: CancellationToken,
    ) -> Result<WorkflowRunReport, WorkflowError> {
        let Self {
            runtime,
            lead,
            caller,
            workflow,
            inputs,
            worker_routes,
            verifier_routes,
            durable_run,
            actor_claim,
        } = self;
        let workflow = Arc::new(workflow);
        let inputs = Arc::new(inputs);
        let worker_routes: Arc<[Option<ResolvedModelRoute>]> = Arc::from(worker_routes);
        let verifier_routes: Arc<[Option<ResolvedModelRoute>]> = Arc::from(verifier_routes);

        let _activity = runtime.turn_runner().begin_root_activity(lead).await?;

        let plan = workflow.plan();
        let name = workflow.definition().name();
        let mut reports: Vec<StageReport> = plan.stages().iter().map(pending_report).collect();
        let mut actors: BTreeMap<String, ManagedMemberHandle> = BTreeMap::new();
        let mut registered: Vec<ManagedMemberHandle> = Vec::new();
        let run_cancel = cancel.child_token();
        let mut running = tokio::task::JoinSet::new();

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
                    let candidates = ready.iter().copied().collect::<Vec<_>>();
                    let mut pending_spawns = Vec::new();
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
                            if let Some(run) = durable_run {
                                append_workflow_event(
                                    runtime.as_ref(),
                                    actor_claim,
                                    ExtensionEvent::WorkflowStageFinished {
                                        session: lead,
                                        run,
                                        stage: stage.id().to_string(),
                                        status: WorkflowStageStatus::Skipped,
                                    },
                                )
                                .await?;
                            }
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
                        let rendered = workflow.render_stage(index, &inputs, &stage_evidence)?;
                        if let Some(run) = durable_run {
                            append_workflow_event(
                                runtime.as_ref(),
                                actor_claim,
                                ExtensionEvent::WorkflowStageStarted {
                                    session: lead,
                                    run,
                                    stage: stage.id().to_string(),
                                },
                            )
                            .await?;
                        }

                        let existing = stage.actor().and_then(|key| actors.get(key)).cloned();
                        let member_handle = if let Some(handle) = existing {
                            handle
                        } else {
                            let label = match stage.actor() {
                                Some(key) => format!("Workflow {name} resident actor `{key}`"),
                                None => format!("Workflow {name} / stage {}", stage.id()),
                            };
                            let spawned = runtime
                                .turn_runner()
                                .spawn_managed_member(ManagedMemberSpawnRequest {
                                    parent: lead,
                                    caller: caller.clone(),
                                    agent: AgentName::new(stage.agent()),
                                    label,
                                    route: worker_routes[index].clone(),
                                    system_context: Some(rendered.system_context().to_string()),
                                    actor_claim,
                                })
                                .await
                                .map_err(map_spawn_host_error)?;
                            registered.push(spawned.clone());
                            if let Some(key) = stage.actor() {
                                actors.insert(key.to_string(), spawned.clone());
                            }
                            spawned
                        };

                        if let Some(run) = durable_run {
                            append_workflow_event(
                                runtime.as_ref(),
                                actor_claim,
                                ExtensionEvent::WorkflowStageMemberLinked {
                                    session: lead,
                                    run,
                                    stage: stage.id().to_string(),
                                    member: member_handle.member,
                                    role: WorkflowMemberRole::Worker,
                                    iteration: 0,
                                },
                            )
                            .await?;
                        }
                        if let Some(key) = stage.actor() {
                            busy_actors.insert(key);
                        }

                        pending_spawns.push((index, member_handle, rendered));
                    }

                    for (index, member_handle, rendered) in pending_spawns {
                        let runtime = Arc::clone(&runtime);
                        let workflow = Arc::clone(&workflow);
                        let caller = caller.clone();
                        let worker_route = worker_routes[index].clone();
                        let verifier_route = verifier_routes[index].clone();
                        let stage_cancel = run_cancel.child_token();

                        running.spawn(async move {
                            let name = workflow.definition().name();
                            let stage = &workflow.plan().stages()[index];
                            let result = async {
                                let mut report = activate_stage(
                                    runtime.as_ref(),
                                    lead,
                                    stage,
                                    &member_handle,
                                    rendered.directive().to_string(),
                                    rendered.system_context().to_string(),
                                    worker_route.clone(),
                                    durable_run,
                                    actor_claim,
                                    stage_cancel.clone(),
                                )
                                .await?;

                                if let Some(verify) = stage.verify()
                                    && report.status == StageStatus::Done
                                {
                                    match drive_loop_stage(
                                        runtime.as_ref(),
                                        lead,
                                        &caller,
                                        name,
                                        stage,
                                        verify,
                                        rendered.directive(),
                                        rendered.system_context(),
                                        rendered
                                            .verification_condition()
                                            .unwrap_or_else(|| verify.until()),
                                        &member_handle,
                                        worker_route,
                                        verifier_route,
                                        report.clone(),
                                        durable_run,
                                        actor_claim,
                                        stage_cancel.clone(),
                                    )
                                    .await
                                    {
                                        Ok(completed) => report = completed,
                                        Err(
                                            error @ (WorkflowError::CapacityExceeded(_)
                                            | WorkflowError::Host(
                                                HostError::Storage(_) | HostError::Unavailable(_),
                                            )),
                                        ) => {
                                            return Err(error);
                                        }
                                        Err(error) => {
                                            report.status = if stage_cancel.is_cancelled() {
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
                            }
                            .await;

                            if stage.actor().is_none() {
                                runtime
                                    .turn_runner()
                                    .end_managed_member(lead, &member_handle.handle)
                                    .await?;
                            }
                            result
                        });
                    }
                }

                let Some(joined) = running.join_next().await else {
                    if admit && !ready.is_empty() {
                        continue;
                    }
                    break;
                };
                let (index, report) = joined.map_err(|error| WorkflowError::Invalid {
                    workflow: name.to_string(),
                    detail: format!("workflow stage task failed: {error}"),
                })??;
                if let Some(key) = plan.stages()[index].actor() {
                    busy_actors.remove(key);
                }
                failed |= report.status == StageStatus::Failed;

                if let Some(run) = durable_run {
                    append_workflow_event(
                        runtime.as_ref(),
                        actor_claim,
                        ExtensionEvent::WorkflowStageFinished {
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

            for report in &mut reports {
                if report.status == StageStatus::Pending {
                    report.status = StageStatus::Skipped;
                    if let Some(run) = durable_run {
                        append_workflow_event(
                            runtime.as_ref(),
                            actor_claim,
                            ExtensionEvent::WorkflowStageFinished {
                                session: lead,
                                run,
                                stage: report.stage.clone(),
                                status: WorkflowStageStatus::Skipped,
                            },
                        )
                        .await?;
                    }
                }
            }
            if run_cancel.is_cancelled() {
                return Ok(WorkflowRunReport {
                    status: WorkflowStatus::Cancelled,
                    stages: reports,
                });
            }
            let status = if reports
                .iter()
                .any(|report| report.status == StageStatus::Failed)
            {
                WorkflowStatus::Failed
            } else {
                WorkflowStatus::Completed
            };
            Ok(WorkflowRunReport {
                status,
                stages: reports,
            })
        }
        .await;

        if result.is_err() {
            run_cancel.cancel();
            while running.join_next().await.is_some() {}
        }
        if !matches!(&result, Ok(report) if report.status == WorkflowStatus::Completed) {
            let endings = futures::future::join_all(registered.iter().map(|member| {
                runtime
                    .turn_runner()
                    .end_managed_member(lead, &member.handle)
            }))
            .await;
            for ending in endings {
                ending?;
            }
        }
        result
    }
}

/// Preflight and validate one compiled Workflow before any child or durable side effects.
///
/// # Errors
/// Returns validation, authorization, route, capacity, or host failures.
pub async fn prepare_workflow_run(
    runtime: Arc<dyn HostRuntime>,
    lead: SessionId,
    caller: AgentName,
    workflow: &CompiledWorkflow,
    inputs: BTreeMap<String, String>,
    durable_run: Option<WorkflowRunId>,
    actor_claim: Option<ActorClaim>,
) -> Result<PreparedWorkflowRun, WorkflowError> {
    workflow.validate_inputs(&inputs)?;
    let plan = workflow.plan();
    let name = workflow.definition().name();
    let wanted = worst_case_admission_units(plan.stages());

    let mut worker_routes = Vec::with_capacity(plan.stages().len());
    let mut verifier_routes = Vec::with_capacity(plan.stages().len());

    for stage in plan.stages() {
        authorize_stage_agent(runtime.as_ref(), lead, &caller, stage.agent()).await?;
        let worker_route = resolve_optional_route(
            runtime.as_ref(),
            name,
            stage.agent(),
            "worker",
            stage.model(),
        )
        .await?;
        worker_routes.push(worker_route);

        let verifier_route = if let Some(verify) = stage.verify() {
            authorize_stage_agent(runtime.as_ref(), lead, &caller, verify.agent()).await?;
            resolve_optional_route(
                runtime.as_ref(),
                name,
                verify.agent(),
                "verifier",
                verify.model(),
            )
            .await?
        } else {
            None
        };
        verifier_routes.push(verifier_route);
    }

    validate_resolved_semantics(name, plan.stages(), &worker_routes)?;
    runtime
        .turn_runner()
        .preflight_member_capacity(lead, wanted)
        .await
        .map_err(|error| map_preflight_capacity_error(name, error))?;

    Ok(PreparedWorkflowRun {
        runtime,
        lead,
        caller,
        workflow: workflow.clone(),
        inputs,
        worker_routes,
        verifier_routes,
        durable_run,
        actor_claim,
    })
}

async fn authorize_stage_agent(
    runtime: &dyn HostRuntime,
    lead: SessionId,
    caller: &AgentName,
    agent_id: &str,
) -> Result<(), WorkflowError> {
    runtime
        .catalog_reader()
        .authorize_workflow_agent(lead, caller, &AgentName::new(agent_id))
        .await
        .map_err(|error| match error {
            HostError::PermissionDenied(detail) | HostError::NotFound(detail) => {
                WorkflowError::Unauthorized {
                    caller: caller.to_string(),
                    agent_id: agent_id.to_string(),
                    detail,
                }
            }
            other => WorkflowError::Host(other),
        })
}

async fn resolve_optional_route(
    runtime: &dyn HostRuntime,
    workflow: &str,
    stage: &str,
    role: &str,
    assignment: Option<&WorkflowModelAssignment>,
) -> Result<Option<ResolvedModelRoute>, WorkflowError> {
    let Some(assignment) = assignment else {
        return Ok(None);
    };
    let mut entries = Vec::with_capacity(1 + assignment.fallback().len());
    entries.push(ModelRouteEntry {
        model: ModelRef::new(assignment.id()),
        reasoning: assignment.reasoning().map(str::to_string),
    });
    for candidate in assignment.fallback() {
        entries.push(ModelRouteEntry {
            model: ModelRef::new(candidate.id()),
            reasoning: candidate.reasoning().map(str::to_string),
        });
    }
    for (index, entry) in entries.iter().enumerate() {
        if entry.model.as_str().contains('#') || entry.model.as_str().trim().is_empty() {
            return Err(WorkflowError::Invalid {
                workflow: workflow.to_string(),
                detail: format!(
                    "Stage `{stage}` {role} route has invalid model id at index {index}"
                ),
            });
        }
    }
    let resolved = runtime
        .model_caller()
        .resolve_model_route(&entries)
        .await
        .map_err(|error| match error {
            HostError::InvalidInput(detail) | HostError::NotFound(detail) => {
                WorkflowError::Invalid {
                    workflow: workflow.to_string(),
                    detail: format!("Stage `{stage}` {role} route: {detail}"),
                }
            }
            other => WorkflowError::Host(other),
        })?;
    if resolved.selected().is_none() {
        return Err(WorkflowError::Invalid {
            workflow: workflow.to_string(),
            detail: format!("Stage `{stage}` {role} route selected no candidate"),
        });
    }
    Ok(Some(resolved))
}

fn validate_resolved_semantics(
    workflow: &str,
    stages: &[WorkflowStage],
    worker_routes: &[Option<ResolvedModelRoute>],
) -> Result<(), WorkflowError> {
    let mut actor_routes: BTreeMap<&str, Option<&ResolvedModelRoute>> = BTreeMap::new();
    for (stage, route) in stages.iter().zip(worker_routes) {
        if let Some(actor) = stage.actor() {
            if let Some(existing) = actor_routes.get(actor) {
                if *existing != route.as_ref() {
                    return Err(WorkflowError::Invalid {
                        workflow: workflow.to_string(),
                        detail: format!(
                            "Stages sharing actor `{actor}` must use an identical effective worker route"
                        ),
                    });
                }
            } else {
                actor_routes.insert(actor, route.as_ref());
            }
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

fn worst_case_admission_units(stages: &[WorkflowStage]) -> u64 {
    stages.iter().fold(0_u64, |total, stage| {
        let units = match stage.verify() {
            Some(verify) => u64::from(verify.max_iterations()).saturating_mul(2),
            None => 1,
        };
        total.saturating_add(units)
    })
}

fn map_preflight_capacity_error(workflow: &str, error: HostError) -> WorkflowError {
    match error {
        HostError::InvalidInput(detail) => {
            if detail.contains("depth") {
                WorkflowError::MaxDepth
            } else {
                WorkflowError::Invalid {
                    workflow: workflow.to_string(),
                    detail,
                }
            }
        }
        HostError::Unavailable(detail) | HostError::Conflict(detail) => {
            WorkflowError::CapacityExceeded(detail)
        }
        other => WorkflowError::Host(other),
    }
}

fn map_spawn_host_error(error: HostError) -> WorkflowError {
    match error {
        HostError::Unavailable(detail) | HostError::Conflict(detail) => {
            WorkflowError::CapacityExceeded(detail)
        }
        other => WorkflowError::Host(other),
    }
}

#[allow(clippy::too_many_arguments)]
async fn activate_stage(
    runtime: &dyn HostRuntime,
    lead: SessionId,
    stage: &WorkflowStage,
    member: &ManagedMemberHandle,
    directive: String,
    system_context: String,
    route: Option<ResolvedModelRoute>,
    durable_run: Option<WorkflowRunId>,
    actor_claim: Option<ActorClaim>,
    cancel: CancellationToken,
) -> Result<StageReport, WorkflowError> {
    if cancel.is_cancelled() {
        return Ok(StageReport {
            stage: stage.id().to_string(),
            agent: stage.agent().to_string(),
            status: StageStatus::Cancelled,
            session: Some(member.session.to_string()),
            output: "Workflow activation cancelled".into(),
        });
    }
    let outcome = runtime
        .turn_runner()
        .activate_managed_member(
            ManagedMemberActivateRequest {
                parent: lead,
                member: member.clone(),
                agent: AgentName::new(stage.agent()),
                directive,
                system_context,
                route,
                actor_claim,
            },
            cancel,
        )
        .await;
    match outcome {
        Ok(outcome) => {
            if let Some(run) = durable_run {
                persist_route_outcomes(
                    runtime,
                    actor_claim,
                    lead,
                    run,
                    stage.id(),
                    member.member,
                    WorkflowMemberRole::Worker,
                    0,
                    &outcome.route_outcomes,
                )
                .await?;
            }
            let status = match outcome.status {
                ManagedMemberActivationStatus::Done => StageStatus::Done,
                ManagedMemberActivationStatus::Failed => StageStatus::Failed,
                ManagedMemberActivationStatus::Cancelled => StageStatus::Cancelled,
            };
            let output = if status == StageStatus::Cancelled && outcome.output_text.is_empty() {
                "Workflow activation cancelled".to_string()
            } else {
                clamp(outcome.output_text)
            };
            Ok(StageReport {
                stage: stage.id().to_string(),
                agent: stage.agent().to_string(),
                status,
                session: Some(member.session.to_string()),
                output,
            })
        }
        Err(HostError::Cancelled) => Ok(StageReport {
            stage: stage.id().to_string(),
            agent: stage.agent().to_string(),
            status: StageStatus::Cancelled,
            session: Some(member.session.to_string()),
            output: "Workflow activation cancelled".into(),
        }),
        Err(error @ (HostError::Storage(_) | HostError::Unavailable(_))) => {
            Err(WorkflowError::Host(error))
        }
        Err(error) => Ok(StageReport {
            stage: stage.id().to_string(),
            agent: stage.agent().to_string(),
            status: StageStatus::Failed,
            session: Some(member.session.to_string()),
            output: clamp(error.to_string()),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive_loop_stage(
    runtime: &dyn HostRuntime,
    lead: SessionId,
    caller: &AgentName,
    workflow_name: &str,
    stage: &WorkflowStage,
    verify: &VerifySpec,
    directive: &str,
    system_context: &str,
    verification_condition: &str,
    worker: &ManagedMemberHandle,
    worker_route: Option<ResolvedModelRoute>,
    verifier_route: Option<ResolvedModelRoute>,
    mut report: StageReport,
    durable_run: Option<WorkflowRunId>,
    actor_claim: Option<ActorClaim>,
    cancel: CancellationToken,
) -> Result<StageReport, WorkflowError> {
    let max_iterations = verify.max_iterations().max(1);
    let mut latest_output = report.output.clone();
    let mut next_directive =
        format!("{directive}\nContinue working toward the verified condition.");
    let label = format!("Workflow {workflow_name} / {}", stage.id());

    for iteration in 0..max_iterations {
        if cancel.is_cancelled() {
            report.status = StageStatus::Cancelled;
            report.output = format!("{}\n\n[loop cancelled]", clamp(latest_output));
            return Ok(report);
        }

        if iteration > 0 {
            if let Some(run) = durable_run {
                append_workflow_event(
                    runtime,
                    actor_claim,
                    ExtensionEvent::WorkflowStageMemberLinked {
                        session: lead,
                        run,
                        stage: stage.id().to_string(),
                        member: worker.member,
                        role: WorkflowMemberRole::Worker,
                        iteration,
                    },
                )
                .await?;
            }
            let worker_outcome = runtime
                .turn_runner()
                .activate_managed_member(
                    ManagedMemberActivateRequest {
                        parent: lead,
                        member: worker.clone(),
                        agent: AgentName::new(stage.agent()),
                        directive: next_directive.clone(),
                        system_context: system_context.to_string(),
                        route: worker_route.clone(),
                        actor_claim,
                    },
                    cancel.clone(),
                )
                .await?;
            if let Some(run) = durable_run {
                persist_route_outcomes(
                    runtime,
                    actor_claim,
                    lead,
                    run,
                    stage.id(),
                    worker.member,
                    WorkflowMemberRole::Worker,
                    iteration,
                    &worker_outcome.route_outcomes,
                )
                .await?;
            }
            match worker_outcome.status {
                ManagedMemberActivationStatus::Done => {
                    latest_output = worker_outcome.output_text;
                }
                ManagedMemberActivationStatus::Cancelled => {
                    report.status = StageStatus::Cancelled;
                    report.output = format!("{}\n\n[loop cancelled]", clamp(latest_output));
                    return Ok(report);
                }
                ManagedMemberActivationStatus::Failed => {
                    report.status = StageStatus::Failed;
                    report.output = clamp(worker_outcome.output_text);
                    return Ok(report);
                }
            }
        }

        // Independent verifier judgment: spawn a fresh verifier member, run it, and end it.
        if cancel.is_cancelled() {
            report.status = StageStatus::Cancelled;
            report.output = format!("{}\n\n[loop cancelled]", clamp(latest_output));
            return Ok(report);
        }
        let verifier_handle = runtime
            .turn_runner()
            .spawn_managed_member(ManagedMemberSpawnRequest {
                parent: lead,
                caller: caller.clone(),
                agent: AgentName::new(verify.agent()),
                label: format!("{label} verify"),
                route: verifier_route.clone(),
                system_context: Some(system_context.to_string()),
                actor_claim,
            })
            .await
            .map_err(map_spawn_host_error)?;

        let verifier_result = async {
            if let Some(run) = durable_run {
                append_workflow_event(
                    runtime,
                    actor_claim,
                    ExtensionEvent::WorkflowStageMemberLinked {
                        session: lead,
                        run,
                        stage: stage.id().to_string(),
                        member: verifier_handle.member,
                        role: WorkflowMemberRole::Verifier,
                        iteration,
                    },
                )
                .await?;
            }
            let judge_directive = format!(
                "You are an independent verifier with no stake in the work. Judge ONLY \
                 whether the condition below is satisfied by the latest worker output.\n\
                 CONDITION: {verification_condition}\n\nLATEST WORKER OUTPUT:\n{}\n\n\
                 Respond with ONLY a JSON object: {{\"met\": true|false, \"reason\": \"...\"}}. \
                 If you cannot see evidence the condition holds, answer met=false.",
                clamp(latest_output.clone())
            );
            let outcome = runtime
                .turn_runner()
                .activate_managed_member(
                    ManagedMemberActivateRequest {
                        parent: lead,
                        member: verifier_handle.clone(),
                        agent: AgentName::new(verify.agent()),
                        directive: judge_directive,
                        system_context: system_context.to_string(),
                        route: verifier_route.clone(),
                        actor_claim,
                    },
                    cancel.clone(),
                )
                .await?;
            if let Some(run) = durable_run {
                persist_route_outcomes(
                    runtime,
                    actor_claim,
                    lead,
                    run,
                    stage.id(),
                    verifier_handle.member,
                    WorkflowMemberRole::Verifier,
                    iteration,
                    &outcome.route_outcomes,
                )
                .await?;
            }
            Ok::<_, WorkflowError>(outcome)
        }
        .await;

        runtime
            .turn_runner()
            .end_managed_member(lead, &verifier_handle.handle)
            .await?;

        let verifier_outcome = verifier_result?;
        match verifier_outcome.status {
            ManagedMemberActivationStatus::Cancelled => {
                report.status = StageStatus::Cancelled;
                report.output = format!("{}\n\n[loop cancelled]", clamp(latest_output));
                return Ok(report);
            }
            ManagedMemberActivationStatus::Failed => {
                report.status = StageStatus::Failed;
                report.output = clamp(verifier_outcome.output_text);
                return Ok(report);
            }
            ManagedMemberActivationStatus::Done => {}
        }

        let verdict = parse_verdict(&verifier_outcome.output_text);
        if verdict.met {
            report.output = format!("{}\n\n[verified: {}]", clamp(latest_output), verdict.reason);
            return Ok(report);
        }
        next_directive = format!(
            "{verification_condition}\n\nThe verifier reports the condition is not yet met: {}\n\
             Continue working toward it.",
            verdict.reason
        );
    }

    report.output = format!(
        "{}\n\n[loop capped after {max_iterations} iterations without verification]",
        clamp(latest_output)
    );
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
async fn persist_route_outcomes(
    runtime: &dyn HostRuntime,
    actor_claim: Option<ActorClaim>,
    session: SessionId,
    run: WorkflowRunId,
    stage: &str,
    member: hya_proto::MemberId,
    role: WorkflowMemberRole,
    iteration: u32,
    outcomes: &[TurnRouteOutcome],
) -> Result<(), WorkflowError> {
    for outcome in outcomes {
        append_workflow_event(
            runtime,
            actor_claim,
            ExtensionEvent::WorkflowStageRouteOutcome {
                session,
                run,
                stage: stage.to_string(),
                member,
                role,
                iteration,
                step: outcome.step,
                candidate_index: outcome.candidate_index,
                model: outcome.model.clone(),
                reasoning: outcome.reasoning.clone(),
                failure_class: map_route_failure_class(outcome.failure_class),
            },
        )
        .await?;
    }
    Ok(())
}

async fn append_workflow_event(
    runtime: &dyn HostRuntime,
    actor_claim: Option<ActorClaim>,
    event: ExtensionEvent,
) -> Result<(), WorkflowError> {
    let (session, bundle, kind, payload) = extension_event_parts(event)?;
    runtime
        .session_store()
        .append_extension_event(session, &bundle, &kind, payload, actor_claim)
        .await?;
    Ok(())
}

const fn map_route_failure_class(class: TurnRouteFailureClass) -> WorkflowRouteFailureClass {
    match class {
        TurnRouteFailureClass::None => WorkflowRouteFailureClass::None,
        TurnRouteFailureClass::Transport => WorkflowRouteFailureClass::Transport,
        TurnRouteFailureClass::RateLimited => WorkflowRouteFailureClass::RateLimited,
        TurnRouteFailureClass::Server => WorkflowRouteFailureClass::Server,
        TurnRouteFailureClass::UnknownModel => WorkflowRouteFailureClass::UnknownModel,
        TurnRouteFailureClass::Auth => WorkflowRouteFailureClass::Auth,
        TurnRouteFailureClass::Incompatible => WorkflowRouteFailureClass::Incompatible,
        TurnRouteFailureClass::Http => WorkflowRouteFailureClass::Http,
        TurnRouteFailureClass::Decode => WorkflowRouteFailureClass::Decode,
        TurnRouteFailureClass::Internal => WorkflowRouteFailureClass::Internal,
        TurnRouteFailureClass::Aborted => WorkflowRouteFailureClass::Aborted,
        TurnRouteFailureClass::Cancelled => WorkflowRouteFailureClass::Cancelled,
    }
}

const fn protocol_stage_status(status: StageStatus) -> WorkflowStageStatus {
    match status {
        StageStatus::Pending => WorkflowStageStatus::Pending,
        StageStatus::Done => WorkflowStageStatus::Completed,
        StageStatus::Failed => WorkflowStageStatus::Failed,
        StageStatus::Cancelled => WorkflowStageStatus::Cancelled,
        StageStatus::Skipped => WorkflowStageStatus::Skipped,
    }
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
    (report.status == StageStatus::Done).then_some(StageEvidence::new(&report.output))
}

/// Clamp predecessor/transcript output to `MAX_PREDECESSOR_OUTPUT_BYTES` on a UTF-8 boundary.
pub(crate) fn clamp(mut text: String) -> String {
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

struct Verdict {
    met: bool,
    reason: String,
}

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
