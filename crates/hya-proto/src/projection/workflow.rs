use super::Projection;
use crate::event::{WorkflowRouteFailureClass, WorkflowStageRouteOutcome};
use crate::ids::{MemberId, OwnerRunId, SessionId, WorkflowRunId};
use crate::model::ModelRef;
use crate::workflow::{
    WorkflowIdentity, WorkflowMemberProjection, WorkflowMemberRole, WorkflowProjection,
    WorkflowRunProjection, WorkflowRunStatus, WorkflowStagePlan, WorkflowStageProjection,
    WorkflowStageStatus,
};

impl Projection {
    /// Return the newest run only when `id` still owns the Session view.
    fn workflow_run_mut(&mut self, id: WorkflowRunId) -> Option<&mut WorkflowRunProjection> {
        self.session
            .workflow
            .as_mut()?
            .run
            .as_mut()
            .filter(|run| run.id == id)
    }

    pub(super) fn apply_workflow_selected(&mut self, workflow: &WorkflowIdentity) {
        self.session
            .workflow
            .get_or_insert_with(WorkflowProjection::default)
            .selection = Some(workflow.clone());
    }

    pub(super) fn apply_workflow_run_started(
        &mut self,
        run: &WorkflowRunId,
        workflow: &WorkflowIdentity,
        request_hash: &str,
        owner: &OwnerRunId,
        stages: &[WorkflowStagePlan],
    ) {
        let projection = self
            .session
            .workflow
            .get_or_insert_with(WorkflowProjection::default);
        if projection.seen_runs.insert(*run) {
            projection.selection = Some(workflow.clone());
            projection.run = Some(WorkflowRunProjection {
                id: *run,
                workflow: workflow.clone(),
                request_hash: request_hash.to_owned(),
                owner: *owner,
                status: WorkflowRunStatus::Running,
                stages: stages
                    .iter()
                    .map(|plan| WorkflowStageProjection {
                        plan: plan.clone(),
                        status: WorkflowStageStatus::Pending,
                        members: Vec::new(),
                        route_outcomes: Vec::new(),
                    })
                    .collect(),
                error: None,
            });
        }
    }

    pub(super) fn apply_workflow_stage_started(&mut self, run: &WorkflowRunId, stage: &str) {
        if let Some(current) = self.workflow_run_mut(*run)
            && current.status == WorkflowRunStatus::Running
            && let Some(existing) = current.stages.iter_mut().find(|row| row.plan.id == *stage)
            && !existing.status.is_terminal()
        {
            existing.status = WorkflowStageStatus::Running;
        }
    }

    pub(super) fn apply_workflow_stage_member_linked(
        &mut self,
        run: &WorkflowRunId,
        stage: &str,
        member: &MemberId,
        role: &WorkflowMemberRole,
        iteration: &u32,
    ) {
        if let Some(current) = self.workflow_run_mut(*run)
            && current.status == WorkflowRunStatus::Running
            && let Some(stage) = current.stages.iter_mut().find(|row| row.plan.id == *stage)
            && !stage.members.iter().any(|row| {
                row.member == *member && row.role == *role && row.iteration == *iteration
            })
        {
            stage.members.push(WorkflowMemberProjection {
                member: *member,
                role: *role,
                iteration: *iteration,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_workflow_stage_route_outcome(
        &mut self,
        session: &SessionId,
        run: &WorkflowRunId,
        stage_id: &str,
        member: &MemberId,
        role: &WorkflowMemberRole,
        iteration: &u32,
        step: &u32,
        candidate_index: &u32,
        model: &ModelRef,
        reasoning: &str,
        failure_class: &WorkflowRouteFailureClass,
    ) {
        if let Some(current) = self.workflow_run_mut(*run)
            && current.status == WorkflowRunStatus::Running
            && let Some(stage) = current
                .stages
                .iter_mut()
                .find(|row| row.plan.id == *stage_id)
            && stage.status == WorkflowStageStatus::Running
            && !stage.route_outcomes.iter().any(|outcome| {
                outcome.stage == *stage_id
                    && outcome.member == *member
                    && outcome.role == *role
                    && outcome.iteration == *iteration
                    && outcome.step == *step
            })
        {
            stage.route_outcomes.push(WorkflowStageRouteOutcome {
                session: *session,
                run: *run,
                stage: stage_id.to_owned(),
                member: *member,
                role: *role,
                iteration: *iteration,
                step: *step,
                candidate_index: *candidate_index,
                model: model.clone(),
                reasoning: reasoning.to_owned(),
                failure_class: *failure_class,
            });
        }
    }

    pub(super) fn apply_workflow_stage_finished(
        &mut self,
        run: &WorkflowRunId,
        stage: &str,
        status: &WorkflowStageStatus,
    ) {
        if status.is_terminal()
            && let Some(current) = self.workflow_run_mut(*run)
            && current.status == WorkflowRunStatus::Running
            && let Some(stage) = current.stages.iter_mut().find(|row| row.plan.id == *stage)
            && !stage.status.is_terminal()
        {
            stage.status = *status;
        }
    }

    pub(super) fn apply_workflow_run_finished(
        &mut self,
        run: &WorkflowRunId,
        status: &WorkflowRunStatus,
        error: &Option<String>,
    ) {
        if status.is_terminal()
            && let Some(current) = self.workflow_run_mut(*run)
            && !current.status.is_terminal()
        {
            current.status = *status;
            current.error = error.clone();
            if *status != WorkflowRunStatus::Completed {
                for stage in &mut current.stages {
                    if stage.status == WorkflowStageStatus::Pending {
                        stage.status = WorkflowStageStatus::Skipped;
                    } else if stage.status == WorkflowStageStatus::Running {
                        stage.status = if *status == WorkflowRunStatus::Cancelled {
                            WorkflowStageStatus::Cancelled
                        } else {
                            WorkflowStageStatus::Failed
                        };
                    }
                }
            }
        }
    }
}
