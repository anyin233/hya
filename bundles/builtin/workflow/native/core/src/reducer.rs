//! ABI v2 [`ProjectionReducer`] for `hya/workflow` extension events.

use hya_proto::{Envelope, Event, SessionId, WorkflowRunId};
use hya_runtime_abi::{ExtensionEventContext, HostError, ProjectionReducer};
use hya_tool::extension::{BUNDLE_WORKFLOW, ExtensionEvent};
use hya_tool::{
    WorkflowMemberProjection, WorkflowProjection, WorkflowRunProjection, WorkflowRunStatus,
    WorkflowStageProjection, WorkflowStageRouteOutcome, WorkflowStageStatus,
};
use serde_json::Value;

/// Deterministic event-log projection reducer for `hya/workflow`.
#[derive(Clone, Debug, Default)]
pub struct WorkflowProjectionReducer;

impl WorkflowProjectionReducer {
    /// Fold one decoded [`ExtensionEvent`] into a [`WorkflowProjection`].
    pub fn fold_event(wf: &mut WorkflowProjection, ext: ExtensionEvent) {
        match ext {
            ExtensionEvent::WorkflowSelected { workflow, .. } => {
                wf.selection = Some(workflow);
            }
            ExtensionEvent::WorkflowRunStarted {
                run,
                workflow,
                request_hash,
                owner,
                stages,
                ..
            } => {
                if wf.seen_runs.insert(run) {
                    wf.selection = Some(workflow.clone());
                    wf.run = Some(WorkflowRunProjection {
                        id: run,
                        workflow,
                        request_hash,
                        owner,
                        status: WorkflowRunStatus::Running,
                        stages: stages
                            .into_iter()
                            .map(|plan| WorkflowStageProjection {
                                plan,
                                status: WorkflowStageStatus::Pending,
                                members: Vec::new(),
                                route_outcomes: Vec::new(),
                            })
                            .collect(),
                        error: None,
                    });
                }
            }
            ExtensionEvent::WorkflowStageStarted { run, stage, .. } => {
                if let Some(current) = wf.run.as_mut().filter(|r| r.id == run)
                    && current.status == WorkflowRunStatus::Running
                    && let Some(existing) =
                        current.stages.iter_mut().find(|row| row.plan.id == stage)
                    && !existing.status.is_terminal()
                {
                    existing.status = WorkflowStageStatus::Running;
                }
            }
            ExtensionEvent::WorkflowStageMemberLinked {
                run,
                stage,
                member,
                role,
                iteration,
                ..
            } => {
                if let Some(current) = wf.run.as_mut().filter(|r| r.id == run)
                    && current.status == WorkflowRunStatus::Running
                    && let Some(stage_row) =
                        current.stages.iter_mut().find(|row| row.plan.id == stage)
                    && !stage_row.members.iter().any(|row| {
                        row.member == member && row.role == role && row.iteration == iteration
                    })
                {
                    stage_row.members.push(WorkflowMemberProjection {
                        member,
                        role,
                        iteration,
                    });
                }
            }
            ExtensionEvent::WorkflowStageRouteOutcome {
                session,
                run,
                stage,
                member,
                role,
                iteration,
                step,
                candidate_index,
                model,
                reasoning,
                failure_class,
            } => {
                if let Some(current) = wf.run.as_mut().filter(|r| r.id == run)
                    && current.status == WorkflowRunStatus::Running
                    && let Some(stage_row) =
                        current.stages.iter_mut().find(|row| row.plan.id == stage)
                    && stage_row.status == WorkflowStageStatus::Running
                    && !stage_row.route_outcomes.iter().any(|outcome| {
                        outcome.stage == stage
                            && outcome.member == member
                            && outcome.role == role
                            && outcome.iteration == iteration
                            && outcome.step == step
                    })
                {
                    stage_row.route_outcomes.push(WorkflowStageRouteOutcome {
                        session,
                        run,
                        stage,
                        member,
                        role,
                        iteration,
                        step,
                        candidate_index,
                        model,
                        reasoning,
                        failure_class,
                    });
                }
            }
            ExtensionEvent::WorkflowStageFinished {
                run, stage, status, ..
            } => {
                if status.is_terminal()
                    && let Some(current) = wf.run.as_mut().filter(|r| r.id == run)
                    && current.status == WorkflowRunStatus::Running
                    && let Some(stage_row) =
                        current.stages.iter_mut().find(|row| row.plan.id == stage)
                    && !stage_row.status.is_terminal()
                {
                    stage_row.status = status;
                }
            }
            ExtensionEvent::WorkflowRunFinished {
                run, status, error, ..
            } => {
                if status.is_terminal()
                    && let Some(current) = wf.run.as_mut().filter(|r| r.id == run)
                    && !current.status.is_terminal()
                {
                    current.status = status;
                    current.error = error;
                    if status != WorkflowRunStatus::Completed {
                        for stage in &mut current.stages {
                            if stage.status == WorkflowStageStatus::Pending {
                                stage.status = WorkflowStageStatus::Skipped;
                            } else if stage.status == WorkflowStageStatus::Running {
                                stage.status = if status == WorkflowRunStatus::Cancelled {
                                    WorkflowStageStatus::Cancelled
                                } else {
                                    WorkflowStageStatus::Failed
                                };
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl ProjectionReducer for WorkflowProjectionReducer {
    fn bundle_id(&self) -> &str {
        BUNDLE_WORKFLOW
    }

    fn initial_state(&self) -> Value {
        Value::Null
    }

    fn apply(&self, state: &mut Value, event: &ExtensionEventContext) -> Result<(), HostError> {
        let Some(ext) =
            ExtensionEvent::from_parts(event.session, BUNDLE_WORKFLOW, &event.kind, &event.payload)
        else {
            return Ok(());
        };
        let mut wf: WorkflowProjection = if state.is_null() {
            WorkflowProjection::default()
        } else {
            serde_json::from_value(state.clone()).unwrap_or_default()
        };
        Self::fold_event(&mut wf, ext);
        *state = serde_json::to_value(&wf)
            .map_err(|err| HostError::Internal(format!("serialize workflow projection: {err}")))?;
        Ok(())
    }
}

/// Return the Workflow run identity carried by one lifecycle event.
#[must_use]
pub fn workflow_event_run(event: &Event) -> Option<WorkflowRunId> {
    match ExtensionEvent::from_event(event)? {
        ExtensionEvent::WorkflowRunStarted { run, .. }
        | ExtensionEvent::WorkflowStageStarted { run, .. }
        | ExtensionEvent::WorkflowStageMemberLinked { run, .. }
        | ExtensionEvent::WorkflowStageRouteOutcome { run, .. }
        | ExtensionEvent::WorkflowStageFinished { run, .. }
        | ExtensionEvent::WorkflowRunFinished { run, .. } => Some(run),
        _ => None,
    }
}

/// Reconstruct one historical run from only the durable events owned by `run`.
#[must_use]
pub fn historical_run(events: &[Envelope], run: WorkflowRunId) -> Option<WorkflowRunProjection> {
    let mut projection = WorkflowProjection::default();
    for envelope in events {
        if workflow_event_run(&envelope.event) != Some(run) {
            continue;
        }
        if let Some(ext) = ExtensionEvent::from_event(&envelope.event) {
            WorkflowProjectionReducer::fold_event(&mut projection, ext);
        }
    }
    projection.run
}

/// Decode [`WorkflowProjection`] from an optional JSON extension value.
#[must_use]
pub fn decode_workflow_projection(value: Option<Value>) -> WorkflowProjection {
    value
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// Extract `(bundle, kind, payload)` from an [`ExtensionEvent`] for `append_extension_event`.
///
/// # Errors
/// Returns [`HostError::Internal`] if the event is not an `Event::Extension`.
pub fn extension_event_parts(
    event: ExtensionEvent,
) -> Result<(SessionId, String, String, Value), HostError> {
    match event.into_event() {
        Event::Extension {
            session,
            bundle,
            kind,
            payload,
        } => Ok((session, bundle, kind, payload)),
        _ => Err(HostError::Internal(
            "workflow extension event did not serialize as Event::Extension".into(),
        )),
    }
}
