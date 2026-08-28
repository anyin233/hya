//! Governed execution and filesystem discovery for compiled Workflows.
//!
//! [`hya_workflow::compile`] owns author parsing and graph normalization. This
//! module accepts only [`CompiledWorkflow`] and applies its immutable levels to
//! the existing Team, iteration, and resident scheduling primitives.

mod run;
mod source;

use thiserror::Error;

pub use hya_workflow::{
    CompiledWorkflow, FailurePolicy, StageMode, VerifySpec, WorkflowDefinition, WorkflowPlan,
    WorkflowRevision, WorkflowStage,
};
pub use run::{
    PreparedWorkflowRun, StageReport, StageStatus, WorkflowRunContext, WorkflowRunReport,
    WorkflowStatus, prepare_workflow_run, prepare_workflow_run_for_actor, run_workflow,
};
pub use source::{discover_workflow_files_in_root, load_workflow_file, workflow_dirs_for_workdir};

/// Failures surfaced by workflow parsing, planning, or execution.
#[derive(Debug, Error)]
pub enum WorkflowError {
    /// A Workflow source could not be read.
    #[error("Workflow source `{source_name}`: {detail}")]
    Source {
        /// Path or source identity.
        source_name: String,
        /// Read failure detail.
        detail: String,
    },
    /// The shared compiler rejected a Workflow document.
    #[error(transparent)]
    Compile(#[from] hya_workflow::WorkflowCompileError),
    /// Runtime values did not satisfy the compiled input/evidence contract.
    #[error(transparent)]
    Render(#[from] hya_workflow::WorkflowRenderError),
    /// The compiled Workflow cannot run under the supplied runtime context.
    #[error("invalid workflow `{workflow}`: {detail}")]
    Invalid {
        /// Workflow name that failed validation.
        workflow: String,
        /// Human-readable rule violation.
        detail: String,
    },
    /// Governed team admission rejected a batch (depth or run budget).
    #[error("workflow admission: {0}")]
    Admission(#[from] crate::subagent::TeamAdmissionError),
    /// The workflow's stage agent id is unknown or outside the caller's
    /// `can_spawn` roster.
    #[error("workflow stage agent `{agent_id}` not spawnable by `{caller}`: {detail}")]
    Unauthorized {
        /// Caller agent id whose roster was consulted.
        caller: String,
        /// Requested stage agent id.
        agent_id: String,
        /// Catalog error detail.
        detail: String,
    },
    /// Engine/store/provider failure during execution.
    #[error(transparent)]
    Engine(#[from] crate::error::CoreError),
}

/// App-facing result of one atomic durable Workflow admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableWorkflowAdmission {
    /// This request durably admitted the run.
    Admitted,
    /// The same immutable run was already admitted.
    Existing,
    /// The run id belongs to different immutable request data.
    Conflict,
    /// Another run owns the Session.
    Busy {
        /// Active run identity.
        run: hya_proto::WorkflowRunId,
    },
}

/// App-facing result of one atomic durable Workflow selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurableWorkflowSelection {
    /// This request changed the durable selection.
    Selected,
    /// A run owns the Session, so selection did not change.
    Busy {
        /// Active run identity.
        run: hya_proto::WorkflowRunId,
    },
}

impl crate::engine::SessionEngine {
    /// Atomically admit one run and publish its start envelope after commit.
    ///
    /// # Errors
    /// Returns a typed core failure for invalid events, stale actor claims, or
    /// persistence failures.
    pub async fn admit_workflow_run(
        &self,
        actor_claim: Option<&hya_proto::ActorClaim>,
        session: hya_proto::SessionId,
        event: hya_proto::Event,
    ) -> Result<DurableWorkflowAdmission, crate::error::CoreError> {
        match self
            .store()
            .admit_workflow_run(actor_claim, session, event)
            .await?
        {
            hya_store::WorkflowAdmissionOutcome::Admitted(envelope) => {
                self.publish_envelope(*envelope);
                Ok(DurableWorkflowAdmission::Admitted)
            }
            hya_store::WorkflowAdmissionOutcome::Existing => Ok(DurableWorkflowAdmission::Existing),
            hya_store::WorkflowAdmissionOutcome::Conflict => Ok(DurableWorkflowAdmission::Conflict),
            hya_store::WorkflowAdmissionOutcome::Busy { run } => {
                Ok(DurableWorkflowAdmission::Busy { run })
            }
        }
    }

    /// Atomically exclude active runs, change selection, and publish after commit.
    ///
    /// # Errors
    /// Returns a typed core failure for invalid events, stale actor claims, or
    /// persistence failures.
    pub async fn select_workflow(
        &self,
        actor_claim: Option<&hya_proto::ActorClaim>,
        session: hya_proto::SessionId,
        event: hya_proto::Event,
    ) -> Result<DurableWorkflowSelection, crate::error::CoreError> {
        match self
            .store()
            .select_workflow(actor_claim, session, event)
            .await?
        {
            hya_store::WorkflowSelectionOutcome::Selected(envelope) => {
                self.publish_envelope(*envelope);
                Ok(DurableWorkflowSelection::Selected)
            }
            hya_store::WorkflowSelectionOutcome::Busy { run } => {
                Ok(DurableWorkflowSelection::Busy { run })
            }
        }
    }

    /// Persist and publish one durable root-owned Workflow control event.
    ///
    /// # Errors
    /// Returns a typed core failure when validation or persistence fails.
    pub async fn record_workflow_event(
        &self,
        session: hya_proto::SessionId,
        event: hya_proto::Event,
    ) -> Result<(), crate::error::CoreError> {
        self.record_workflow_event_for_actor(None, session, event)
            .await
    }

    /// Persist and publish one durable Workflow event under an optional actor fence.
    ///
    /// # Errors
    /// Returns a typed core failure when the event is not a Workflow event, its
    /// Session does not match, the actor claim is stale, or persistence fails.
    pub async fn record_workflow_event_for_actor(
        &self,
        actor_claim: Option<&hya_proto::ActorClaim>,
        session: hya_proto::SessionId,
        event: hya_proto::Event,
    ) -> Result<(), crate::error::CoreError> {
        if !matches!(
            &event,
            hya_proto::Event::WorkflowSelected { .. }
                | hya_proto::Event::WorkflowRunStarted { .. }
                | hya_proto::Event::WorkflowStageStarted { .. }
                | hya_proto::Event::WorkflowStageMemberLinked { .. }
                | hya_proto::Event::WorkflowStageFinished { .. }
                | hya_proto::Event::WorkflowRunFinished { .. }
        ) || event.session() != Some(session)
        {
            return Err(crate::error::CoreError::Invalid(
                "Workflow event Session mismatch or unsupported event".to_string(),
            ));
        }
        self.emit_for_actor(actor_claim, session, event).await
    }
}
