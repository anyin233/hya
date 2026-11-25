use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{
    ActorClaim, Event, MemberId, ReportOutcome, SessionId, WorkflowMemberRole, WorkflowRunId,
};
use hya_workflow::{MAX_PREDECESSOR_OUTPUT_BYTES, VerifySpec, WorkflowStage};
use tokio_util::sync::CancellationToken;

use crate::completion::{
    GateOutcome, IterationDriver, IterationExecutor, IterationGate, SafetyCaps,
};
use crate::engine::{AgentSpec, SessionEngine};
use crate::error::CoreError;
use crate::resident::{ActivationResult, ResidentSupervisor, ResolvedResidentRuntime};

use super::super::{
    WorkflowError, WorkflowModelRoute, WorkflowRouteRecorder, WorkflowTurnRoute,
    WorkflowTurnRouteSpec,
};
use super::preflight::ResolvedAgent;
use super::{StageReport, StageStatus, WorkflowRunContext};

#[derive(Clone)]
pub(super) struct ResidentActor {
    pub(super) session: SessionId,
    pub(super) handle: String,
    pub(super) member: MemberId,
}

pub(super) struct ResidentActivation {
    pub(super) resolved: (AgentSpec, ResolvedResidentRuntime),
    pub(super) actor: ResidentActor,
    pub(super) directive: String,
    pub(super) system_context: Arc<str>,
    pub(super) cancel: CancellationToken,
    pub(super) actor_claim: Option<ActorClaim>,
    pub(super) route: Option<WorkflowTurnRoute>,
}

/// The controller accepts both at-rest settlement and a terminal report.
pub(super) async fn activate_resident_stage(
    engine: Arc<SessionEngine>,
    lead: SessionId,
    supervisor: Arc<ResidentSupervisor>,
    stage: &WorkflowStage,
    activation: ResidentActivation,
) -> Result<StageReport, WorkflowError> {
    let ResidentActivation {
        resolved,
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
        resolved,
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
        Err(error @ CoreError::Store(_)) => {
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
    resolved: (AgentSpec, ResolvedResidentRuntime),
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
            resolved,
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
        ActivationResult::Reported { text, .. } => Err(CoreError::WorkflowActivationFailed(text)),
        ActivationResult::Ended { note, .. } => Err(CoreError::WorkflowActivationFailed(note)),
    }
}

pub(super) fn clamp(mut text: String) -> String {
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
pub(super) async fn drive_loop_stage(
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
            resolved: (
                resolved_worker.spec.clone(),
                (
                    ctx.binding.clone(),
                    resolved_worker.agents.clone(),
                    resolved_worker.resources.clone(),
                    resolved_worker.sidecar_factory.clone(),
                ),
            ),
            route: resolved_worker.route.clone(),
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
            cancel,
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
    resolved: (AgentSpec, ResolvedResidentRuntime),
    engine: Arc<SessionEngine>,
    lead: SessionId,
    supervisor: Arc<ResidentSupervisor>,
    worker: ResidentActor,
    route: Option<WorkflowModelRoute>,
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
        cancel: &CancellationToken,
    ) -> Result<String, CoreError> {
        if cancel.is_cancelled() {
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
                .await?;
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
            self.resolved.clone(),
            directive.to_string(),
            self.guidance.clone(),
            route,
            self.actor_claim.as_ref(),
            cancel,
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
    guidance: Arc<str>,
    actor_claim: Option<ActorClaim>,
    durable_run: Option<WorkflowRunId>,
    route_recorder: Option<WorkflowRouteRecorder>,
    stage_id: String,
    next_iteration: tokio::sync::Mutex<u32>,
}

#[async_trait]
impl IterationGate for VerifierGate {
    async fn judge(
        &self,
        transcript: &str,
        cancel: &CancellationToken,
    ) -> Result<GateOutcome, CoreError> {
        let directive = format!(
            "You are an independent verifier with no stake in the work. Judge ONLY \
             whether the condition below is satisfied by the latest worker output.\n\
             CONDITION: {}\n\nLATEST WORKER OUTPUT:\n{}\n\nRespond with ONLY a JSON \
             object: {{\"met\": true|false, \"reason\": \"...\"}}. If you cannot see \
             evidence the condition holds, answer met=false.",
            self.until,
            render_transcript_bounded(transcript)
        );
        if cancel.is_cancelled() {
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
                (
                    self.verifier.spec.clone(),
                    (
                        self.ctx.binding.clone(),
                        self.verifier.agents.clone(),
                        self.verifier.resources.clone(),
                        self.verifier.sidecar_factory.clone(),
                    ),
                ),
                directive,
                self.guidance.clone(),
                route,
                self.actor_claim.as_ref(),
                cancel,
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
