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

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::SpawnLifecycle;
use hya_proto::{MailEndpoint, MailKind, MemberId, RosterStatus, SessionId};
use hya_tool::AgentDef;
use hya_workflow::{
    CompiledWorkflow, FailurePolicy, MAX_PREDECESSOR_OUTPUT_BYTES, StageEvidence,
    StageEvidenceStatus, StageMode, VerifySpec, WorkflowStage,
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
use crate::subagent::{MemberEvidence, MemberSpec, MemberStatus, run_pre_admitted_team};

use super::WorkflowError;

/// Per-run inputs for executing a workflow.
///
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
}

#[derive(Clone)]
struct ResidentActor {
    session: SessionId,
    handle: String,
}

struct PreparedActivation {
    index: usize,
    directive: String,
    system_context: Arc<str>,
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

fn resolve_agent(
    engine: &SessionEngine,
    ctx: &WorkflowRunContext,
    agent_id: &str,
) -> Result<ResolvedAgent, WorkflowError> {
    let definition = ctx
        .binding
        .resolve_spawn(&ctx.caller, agent_id)
        .map_err(|error| WorkflowError::Unauthorized {
            caller: ctx.caller.clone(),
            agent_id: agent_id.to_string(),
            detail: error.to_string(),
        })?;
    let spec =
        engine.agent_spec_for_binding(&ctx.binding, &ctx.base_agent, definition.stable_id)?;
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
    })
}

/// Execute `def` under `lead`, honoring `ctx`.
///
/// # Errors
/// [`WorkflowError`] for invalid definitions, unauthorized/unknown stage
/// agents, admission rejections, and engine failures during member turns.
pub async fn run_workflow(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    workflow: &CompiledWorkflow,
    ctx: WorkflowRunContext,
    cancel: CancellationToken,
) -> Result<WorkflowRunReport, WorkflowError> {
    workflow.validate_inputs(&ctx.inputs)?;
    let plan = workflow.plan();
    let name = workflow.definition().name();

    let wanted = worst_case_admission_units(plan.stages());

    let mut resolved = Vec::with_capacity(plan.stages().len());
    let mut verifiers = Vec::with_capacity(plan.stages().len());
    for stage in plan.stages() {
        resolved.push(resolve_agent(&engine, &ctx, stage.agent())?);
        verifiers.push(
            stage
                .verify()
                .map(|verify| resolve_agent(&engine, &ctx, verify.agent()))
                .transpose()?,
        );
    }
    validate_resolved_semantics(name, plan.stages(), &resolved, &verifiers, &ctx)?;
    let _budget = reserve_workflow_budget(&engine, lead, name, wanted).await?;

    let mut reports: Vec<StageReport> = plan.stages().iter().map(pending_report).collect();
    let mut actors: BTreeMap<String, ResidentActor> = BTreeMap::new();
    for level in plan.levels() {
        if cancel.is_cancelled() {
            mark_pending(&mut reports, StageStatus::Cancelled);
            return Ok(WorkflowRunReport {
                status: WorkflowStatus::Cancelled,
                stages: reports,
            });
        }

        let stage_evidence = reports.iter().map(report_evidence).collect::<Vec<_>>();
        let mut prepared = Vec::with_capacity(level.stage_indices().len());
        for &index in level.stage_indices() {
            let rendered = workflow.render_stage(index, &ctx.inputs, &stage_evidence)?;
            prepared.push(PreparedActivation {
                index,
                directive: rendered.directive().to_string(),
                system_context: Arc::from(rendered.system_context().to_string()),
            });
        }

        let mut transient_indices = Vec::new();
        let mut transient_specs = Vec::new();
        let mut resident_activations = Vec::new();
        for activation in prepared {
            let stage = &plan.stages()[activation.index];
            let resolved_stage = resolved[activation.index].clone();
            if let Some(actor_key) = stage.actor() {
                let Some(supervisor) = ctx.resident_supervisor.as_ref() else {
                    return Err(WorkflowError::Invalid {
                        workflow: name.to_string(),
                        detail: "resident supervisor disappeared after preflight".to_string(),
                    });
                };
                let actor = if let Some(existing) = actors.get(actor_key).cloned() {
                    existing
                } else {
                    let (session, handle) = supervisor
                        .spawn_resident_parked(
                            lead,
                            resolved_stage.spec.clone(),
                            (
                                ctx.binding.clone(),
                                resolved_stage.agents.clone(),
                                resolved_stage.resources.clone(),
                                resolved_stage.sidecar_factory.clone(),
                            ),
                            format!("Workflow {name} resident actor `{actor_key}`"),
                            None,
                            Some(activation.system_context.clone()),
                        )
                        .await?;
                    let actor = ResidentActor { session, handle };
                    actors.insert(actor_key.to_string(), actor.clone());
                    actor
                };
                resident_activations.push((activation, actor));
            } else {
                transient_indices.push(activation.index);
                transient_specs.push(MemberSpec {
                    id: MemberId::new(),
                    agent: resolved_stage.spec,
                    binding: ctx.binding.clone(),
                    agents: resolved_stage.agents,
                    resources: Some(resolved_stage.resources),
                    guidance: Some(activation.system_context),
                    directive: activation.directive,
                    tool_call: None,
                    description: format!("Workflow {name} / {}", stage.id()),
                    session: None,
                    sidecar_factory: resolved_stage.sidecar_factory,
                });
            }
        }

        let transient_run = async {
            if transient_specs.is_empty() {
                Vec::new()
            } else {
                run_pre_admitted_team(engine.clone(), lead, transient_specs, cancel.child_token())
                    .await
            }
        };
        let resident_run = futures::future::join_all(resident_activations.into_iter().map(
            |(activation, actor)| {
                let engine = engine.clone();
                let supervisor = ctx.resident_supervisor.as_ref().cloned();
                let stage = plan.stages()[activation.index].clone();
                let activation_cancel = cancel.child_token();
                async move {
                    let Some(supervisor) = supervisor else {
                        return Err(WorkflowError::Invalid {
                            workflow: name.to_string(),
                            detail: "resident supervisor disappeared after spawn".to_string(),
                        });
                    };
                    let index = activation.index;
                    let report = activate_resident_stage(
                        engine,
                        lead,
                        supervisor,
                        &stage,
                        actor,
                        activation,
                        activation_cancel,
                    )
                    .await?;
                    Ok::<_, WorkflowError>((index, report))
                }
            },
        ));
        let (evidence, resident_results) = tokio::join!(transient_run, resident_run);
        let cancelled = cancel.is_cancelled();
        for (&index, evidence) in transient_indices.iter().zip(evidence.iter()) {
            reports[index] =
                stage_report(&engine, &plan.stages()[index], evidence, cancelled).await?;
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
            let rendered = workflow.render_stage(index, &ctx.inputs, &stage_evidence)?;
            let verification_condition = rendered
                .verification_condition()
                .unwrap_or_else(|| verify.until());
            let mut report = reports[index].clone();
            match drive_loop_stage(
                engine.clone(),
                lead,
                name,
                &ctx,
                stage,
                verify,
                rendered.directive(),
                rendered.system_context(),
                verification_condition,
                &resolved[index],
                verifier,
                report.clone(),
                cancel.clone(),
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
    Ok(WorkflowRunReport {
        status,
        stages: reports,
    })
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
    for ((stage, worker), verifier) in stages.iter().zip(resolved).zip(verifiers) {
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
    actor: ResidentActor,
    activation: PreparedActivation,
    cancel: CancellationToken,
) -> Result<StageReport, WorkflowError> {
    let PreparedActivation {
        directive,
        system_context,
        ..
    } = activation;
    supervisor
        .set_resident_guidance(actor.session, system_context)
        .await?;
    let mut events = engine.bus().subscribe();
    engine
        .mail_send(
            lead,
            MailEndpoint::Handle(actor.handle.clone()),
            MailKind::Message,
            directive,
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
            *self.latest_output.lock().await = first.clone();
            return Ok(first);
        }
        let resumed = *self.session.lock().await;
        let spec = MemberSpec {
            id: MemberId::new(),
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
        let evidence = run_pre_admitted_team(
            self.engine.clone(),
            self.lead,
            vec![spec],
            cancel.child_token(),
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
        let spec = MemberSpec {
            id: MemberId::new(),
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
        let evidence = run_pre_admitted_team(
            self.engine.clone(),
            self.lead,
            vec![spec],
            self.cancel.child_token(),
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
