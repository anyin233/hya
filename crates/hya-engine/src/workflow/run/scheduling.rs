use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use hya_proto::{ActorClaim, SessionId};
use hya_tool::extension::ExtensionEvent;
use hya_tool::{WorkflowMemberRole, WorkflowStageStatus};
use hya_workflow::{FailurePolicy, StageEvidence, WorkflowStage};
use tokio_util::sync::CancellationToken;

use crate::engine::SessionEngine;
use crate::error::CoreError;

use super::super::{
    WorkflowError, WorkflowModelRoute, WorkflowRouteRecord, WorkflowRouteRecorder,
    WorkflowTurnRoute, WorkflowTurnRouteSpec,
};
use super::preflight::ResolvedAgent;
use super::stage::{
    ResidentActivation, ResidentActor, activate_resident_stage, clamp, drive_loop_stage,
};
use super::{PreparedWorkflowRun, StageReport, StageStatus, WorkflowRunReport, WorkflowStatus};

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
            #[cfg(test)]
            stage_hold,
        } = self;
        let workflow = Arc::new(workflow);
        let context = Arc::new(context);
        let resolved: Arc<[ResolvedAgent]> = Arc::from(resolved);
        let verifiers: Arc<[Option<ResolvedAgent>]> = Arc::from(verifiers);
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
                    // A ready actor is reserved through its complete Stage (including
                    // verification). Compiled indices break ties between ready uses.
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
                                engine
                                    .record_workflow_event_for_actor(
                                        actor_claim.as_ref(),
                                        lead,
                                        ExtensionEvent::WorkflowStageFinished {
                                            session: lead,
                                            run,
                                            stage: stage.id().to_string(),
                                            status: WorkflowStageStatus::Skipped,
                                        }
                                        .into_event(),
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
                        let rendered =
                            workflow.render_stage(index, &context.inputs, &stage_evidence)?;
                        if let Some(run) = durable_run {
                            engine
                                .record_workflow_event_for_actor(
                                    actor_claim.as_ref(),
                                    lead,
                                    ExtensionEvent::WorkflowStageStarted {
                                        session: lead,
                                        run,
                                        stage: stage.id().to_string(),
                                    }
                                    .into_event(),
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
                                    ExtensionEvent::WorkflowStageMemberLinked {
                                        session: lead,
                                        run,
                                        stage: stage.id().to_string(),
                                        member: actor.member,
                                        role: WorkflowMemberRole::Worker,
                                        iteration: 0,
                                    }
                                    .into_event(),
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
                            resolved: (
                                resolved[index].spec.clone(),
                                (
                                    context.binding.clone(),
                                    resolved[index].agents.clone(),
                                    resolved[index].resources.clone(),
                                    resolved[index].sidecar_factory.clone(),
                                ),
                            ),
                            actor: actor.clone(),
                            directive: rendered.directive().to_string(),
                            system_context: Arc::from(rendered.system_context()),
                            cancel: run_cancel.child_token(),
                            actor_claim,
                            route,
                        };
                        pending_spawns.push((
                            index,
                            supervisor.clone(),
                            actor,
                            activation,
                            rendered,
                            verifiers[index].clone(),
                        ));
                    }
                    // Register every ready candidate before spawning any activation so a
                    // registration failure mid-batch cannot race ahead with an earlier Stage.
                    for (index, supervisor, actor, activation, rendered, verifier) in pending_spawns
                    {
                        let engine = engine.clone();
                        let workflow = Arc::clone(&workflow);
                        let context = Arc::clone(&context);
                        let resolved = Arc::clone(&resolved);
                        let run_cancel = run_cancel.clone();
                        let route_recorder = route_recorder.clone();
                        #[cfg(test)]
                        let stage_hold = stage_hold.clone();
                        running.spawn(async move {
                            let name = workflow.definition().name();
                            let stage = &workflow.plan().stages()[index];
                            let result = async {
                                let mut report = activate_resident_stage(
                                    engine.clone(),
                                    lead,
                                    supervisor.clone(),
                                    stage,
                                    activation,
                                )
                                .await?;
                                if let Some((verify, verifier)) = stage.verify().zip(verifier)
                                    && report.status == StageStatus::Done
                                {
                                    match drive_loop_stage(
                                        engine.clone(),
                                        lead,
                                        name,
                                        &context,
                                        stage,
                                        verify,
                                        rendered.directive(),
                                        rendered.system_context(),
                                        rendered
                                            .verification_condition()
                                            .unwrap_or_else(|| verify.until()),
                                        actor.clone(),
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
                                        Err(WorkflowError::Engine(
                                            error @ (CoreError::Store(_)
                                            | CoreError::CapacityExceeded { .. }),
                                        )) => {
                                            return Err(error.into());
                                        }
                                        Err(WorkflowError::Engine(
                                            CoreError::WorkflowActivationFailed(text),
                                        )) => {
                                            report.status = StageStatus::Failed;
                                            report.output = clamp(text);
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
                            }
                            .await;
                            if stage.actor().is_none() {
                                let (root, _) = engine.session_lineage(lead).await?;
                                supervisor.end_member(root, &actor.handle).await?;
                            }
                            #[cfg(test)]
                            if result.is_ok()
                                && let Some(hold) = stage_hold.as_ref()
                            {
                                if stage.id() == hold.hold_stage {
                                    hold.both_activated.wait().await;
                                    let tx = engine
                                        .store()
                                        .begin_transaction()
                                        .await
                                        .map_err(CoreError::Store)?;
                                    hold.tx_acquired.notify_one();
                                    hold.release_tx.notified().await;
                                    drop(tx);
                                } else if stage.id() == hold.fast_stage {
                                    hold.both_activated.wait().await;
                                    hold.tx_acquired.notified().await;
                                }
                            }
                            result
                        });
                    }
                }
                let Some(joined) = running.join_next().await else {
                    // Skipping a blocked Stage can expose another blocked successor.
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
                #[cfg(test)]
                if let Some(hold) = stage_hold.as_ref()
                    && report.stage == hold.fast_stage
                {
                    hold.release_tx.notify_one();
                }
                if let Some(run) = durable_run {
                    engine
                        .record_workflow_event_for_actor(
                            actor_claim.as_ref(),
                            lead,
                            ExtensionEvent::WorkflowStageFinished {
                                session: lead,
                                run,
                                stage: report.stage.clone(),
                                status: protocol_stage_status(report.status),
                            }
                            .into_event(),
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
                        engine
                            .record_workflow_event_for_actor(
                                actor_claim.as_ref(),
                                lead,
                                ExtensionEvent::WorkflowStageFinished {
                                    session: lead,
                                    run,
                                    stage: report.stage.clone(),
                                    status: WorkflowStageStatus::Skipped,
                                }
                                .into_event(),
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
            while running.join_next().await.is_some() {}
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
