//! Durable `hya/workflow` extension events and their `Event::Extension` wire form.

use hya_proto::{Event, MemberId, ModelRef, OwnerRunId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{
    WorkflowIdentity, WorkflowMemberRole, WorkflowRouteFailureClass, WorkflowRunId,
    WorkflowRunStatus, WorkflowStagePlan, WorkflowStageStatus,
};
use crate::service::WORKFLOW_BUNDLE_ID;

/// One durable Workflow lifecycle event appended to a Session log.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum WorkflowEvent {
    WorkflowSelected {
        session: SessionId,
        workflow: WorkflowIdentity,
    },
    WorkflowRunStarted {
        session: SessionId,
        run: WorkflowRunId,
        workflow: WorkflowIdentity,
        request_hash: String,
        owner: OwnerRunId,
        stages: Vec<WorkflowStagePlan>,
    },
    WorkflowStageStarted {
        session: SessionId,
        run: WorkflowRunId,
        stage: String,
    },
    WorkflowStageMemberLinked {
        session: SessionId,
        run: WorkflowRunId,
        stage: String,
        member: MemberId,
        role: WorkflowMemberRole,
        iteration: u32,
    },
    WorkflowStageRouteOutcome {
        session: SessionId,
        run: WorkflowRunId,
        stage: String,
        member: MemberId,
        role: WorkflowMemberRole,
        iteration: u32,
        step: u32,
        candidate_index: u32,
        model: ModelRef,
        reasoning: String,
        failure_class: WorkflowRouteFailureClass,
    },
    WorkflowStageFinished {
        session: SessionId,
        run: WorkflowRunId,
        stage: String,
        status: WorkflowStageStatus,
    },
    WorkflowRunFinished {
        session: SessionId,
        run: WorkflowRunId,
        status: WorkflowRunStatus,
        error: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
struct WorkflowSelectedWire {
    workflow: WorkflowIdentity,
}

#[derive(Serialize, Deserialize)]
struct WorkflowRunStartedWire {
    run: WorkflowRunId,
    workflow: WorkflowIdentity,
    request_hash: String,
    owner: OwnerRunId,
    stages: Vec<WorkflowStagePlan>,
}

#[derive(Serialize, Deserialize)]
struct WorkflowStageStartedWire {
    run: WorkflowRunId,
    stage: String,
}

#[derive(Serialize, Deserialize)]
struct WorkflowStageMemberLinkedWire {
    run: WorkflowRunId,
    stage: String,
    member: MemberId,
    role: WorkflowMemberRole,
    iteration: u32,
}

#[derive(Serialize, Deserialize)]
struct WorkflowStageRouteOutcomeWire {
    run: WorkflowRunId,
    stage: String,
    member: MemberId,
    role: WorkflowMemberRole,
    iteration: u32,
    step: u32,
    candidate_index: u32,
    model: ModelRef,
    reasoning: String,
    failure_class: WorkflowRouteFailureClass,
}

#[derive(Serialize, Deserialize)]
struct WorkflowStageFinishedWire {
    run: WorkflowRunId,
    stage: String,
    status: WorkflowStageStatus,
}

#[derive(Serialize, Deserialize)]
struct WorkflowRunFinishedWire {
    run: WorkflowRunId,
    status: WorkflowRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl WorkflowEvent {
    /// Extension event kind tag for [`Event::Extension::kind`].
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::WorkflowSelected { .. } => "workflow_selected",
            Self::WorkflowRunStarted { .. } => "workflow_run_started",
            Self::WorkflowStageStarted { .. } => "workflow_stage_started",
            Self::WorkflowStageMemberLinked { .. } => "workflow_stage_member_linked",
            Self::WorkflowStageRouteOutcome { .. } => "workflow_stage_route_outcome",
            Self::WorkflowStageFinished { .. } => "workflow_stage_finished",
            Self::WorkflowRunFinished { .. } => "workflow_run_finished",
        }
    }

    /// Owning session id.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        match self {
            Self::WorkflowSelected { session, .. }
            | Self::WorkflowRunStarted { session, .. }
            | Self::WorkflowStageStarted { session, .. }
            | Self::WorkflowStageMemberLinked { session, .. }
            | Self::WorkflowStageRouteOutcome { session, .. }
            | Self::WorkflowStageFinished { session, .. }
            | Self::WorkflowRunFinished { session, .. } => *session,
        }
    }

    /// Workflow run carried by a run lifecycle event (every kind but selection).
    #[must_use]
    pub const fn run(&self) -> Option<WorkflowRunId> {
        match self {
            Self::WorkflowSelected { .. } => None,
            Self::WorkflowRunStarted { run, .. }
            | Self::WorkflowStageStarted { run, .. }
            | Self::WorkflowStageMemberLinked { run, .. }
            | Self::WorkflowStageRouteOutcome { run, .. }
            | Self::WorkflowStageFinished { run, .. }
            | Self::WorkflowRunFinished { run, .. } => Some(*run),
        }
    }

    /// Encode the event payload of [`Event::Extension`].
    ///
    /// # Errors
    /// Returns the serializer error for a payload that cannot become JSON.
    pub fn payload(self) -> Result<Value, serde_json::Error> {
        match self {
            Self::WorkflowSelected { workflow, .. } => {
                serde_json::to_value(WorkflowSelectedWire { workflow })
            }
            Self::WorkflowRunStarted {
                run,
                workflow,
                request_hash,
                owner,
                stages,
                ..
            } => serde_json::to_value(WorkflowRunStartedWire {
                run,
                workflow,
                request_hash,
                owner,
                stages,
            }),
            Self::WorkflowStageStarted { run, stage, .. } => {
                serde_json::to_value(WorkflowStageStartedWire { run, stage })
            }
            Self::WorkflowStageMemberLinked {
                run,
                stage,
                member,
                role,
                iteration,
                ..
            } => serde_json::to_value(WorkflowStageMemberLinkedWire {
                run,
                stage,
                member,
                role,
                iteration,
            }),
            Self::WorkflowStageRouteOutcome {
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
                ..
            } => serde_json::to_value(WorkflowStageRouteOutcomeWire {
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
            }),
            Self::WorkflowStageFinished {
                run, stage, status, ..
            } => serde_json::to_value(WorkflowStageFinishedWire { run, stage, status }),
            Self::WorkflowRunFinished {
                run, status, error, ..
            } => serde_json::to_value(WorkflowRunFinishedWire { run, status, error }),
        }
    }

    /// Decode a `hya/workflow` [`Event::Extension`]; `None` for any other event.
    #[must_use]
    pub fn from_event(event: &Event) -> Option<Self> {
        let Event::Extension {
            session,
            bundle,
            kind,
            payload,
        } = event
        else {
            return None;
        };
        if bundle != WORKFLOW_BUNDLE_ID {
            return None;
        }
        Self::from_parts(*session, kind, payload)
    }

    /// Decode one `hya/workflow` event `kind` and `payload`; `None` for an
    /// unknown kind or a payload that does not match it.
    #[must_use]
    pub fn from_parts(session: SessionId, kind: &str, payload: &Value) -> Option<Self> {
        let payload = payload.clone();
        match kind {
            "workflow_selected" => {
                let wire: WorkflowSelectedWire = serde_json::from_value(payload).ok()?;
                Some(Self::WorkflowSelected {
                    session,
                    workflow: wire.workflow,
                })
            }
            "workflow_run_started" => {
                let wire: WorkflowRunStartedWire = serde_json::from_value(payload).ok()?;
                Some(Self::WorkflowRunStarted {
                    session,
                    run: wire.run,
                    workflow: wire.workflow,
                    request_hash: wire.request_hash,
                    owner: wire.owner,
                    stages: wire.stages,
                })
            }
            "workflow_stage_started" => {
                let wire: WorkflowStageStartedWire = serde_json::from_value(payload).ok()?;
                Some(Self::WorkflowStageStarted {
                    session,
                    run: wire.run,
                    stage: wire.stage,
                })
            }
            "workflow_stage_member_linked" => {
                let wire: WorkflowStageMemberLinkedWire = serde_json::from_value(payload).ok()?;
                Some(Self::WorkflowStageMemberLinked {
                    session,
                    run: wire.run,
                    stage: wire.stage,
                    member: wire.member,
                    role: wire.role,
                    iteration: wire.iteration,
                })
            }
            "workflow_stage_route_outcome" => {
                let wire: WorkflowStageRouteOutcomeWire = serde_json::from_value(payload).ok()?;
                Some(Self::WorkflowStageRouteOutcome {
                    session,
                    run: wire.run,
                    stage: wire.stage,
                    member: wire.member,
                    role: wire.role,
                    iteration: wire.iteration,
                    step: wire.step,
                    candidate_index: wire.candidate_index,
                    model: wire.model,
                    reasoning: wire.reasoning,
                    failure_class: wire.failure_class,
                })
            }
            "workflow_stage_finished" => {
                let wire: WorkflowStageFinishedWire = serde_json::from_value(payload).ok()?;
                Some(Self::WorkflowStageFinished {
                    session,
                    run: wire.run,
                    stage: wire.stage,
                    status: wire.status,
                })
            }
            "workflow_run_finished" => {
                let wire: WorkflowRunFinishedWire = serde_json::from_value(payload).ok()?;
                Some(Self::WorkflowRunFinished {
                    session,
                    run: wire.run,
                    status: wire.status,
                    error: wire.error,
                })
            }
            _ => None,
        }
    }
}
