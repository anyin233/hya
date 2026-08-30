//! Governed execution and filesystem discovery for compiled Workflows.
//!
//! [`hya_workflow::compile`] owns author parsing and graph normalization. This
//! module accepts only [`CompiledWorkflow`] and applies its immutable levels to
//! the existing Team, iteration, and resident scheduling primitives.

mod run;
mod source;
use std::sync::{Arc, Mutex};

use hya_proto::{Event, MemberId, SessionId, WorkflowMemberRole, WorkflowRunId};
use hya_provider::ReasoningEffort;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

pub use hya_workflow::{
    CompiledWorkflow, FailurePolicy, StageMode, VerifySpec, WorkflowDefinition,
    WorkflowModelAssignment, WorkflowModelCandidate, WorkflowPlan, WorkflowRevision, WorkflowStage,
};
pub use run::{
    PreparedWorkflowRun, StageReport, StageStatus, WorkflowRunContext, WorkflowRunReport,
    WorkflowStatus, prepare_workflow_run, prepare_workflow_run_for_actor, run_workflow,
};
pub use source::{discover_workflow_files_in_root, load_workflow_file, workflow_dirs_for_workdir};

/// One runtime-resolved candidate in an explicit Workflow route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowModelRouteCandidate {
    /// Base model reference; Workflow routes never encode effort in a suffix.
    pub model: hya_proto::ModelRef,
    /// Effective typed effort for this candidate.
    pub reasoning: ReasoningEffort,
}

/// Immutable ordered route selected during Workflow admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowModelRoute {
    /// Full declaration order, including unknown/unroutable tail entries.
    pub candidates: Arc<[WorkflowModelRouteCandidate]>,
    /// First candidate whose provider route was available at admission.
    pub selected_index: usize,
}

impl WorkflowModelRoute {
    /// Return the candidate selected during admission.
    #[must_use]
    pub fn selected(&self) -> Option<&WorkflowModelRouteCandidate> {
        self.candidates.get(self.selected_index)
    }
}

/// Provider/category context captured for one Workflow run.
#[derive(Clone)]
pub struct WorkflowRoutingContext {
    /// Category registry used by the normal Agent policy.
    pub categories: Arc<crate::category::CategoryRegistry>,
    /// Provider router used for defaults, capability, and admission checks.
    pub router: Arc<hya_provider::ProviderRouter>,
}

impl WorkflowRoutingContext {
    /// Build a routing context from the already configured process surfaces.
    #[must_use]
    pub fn new(
        categories: Arc<crate::category::CategoryRegistry>,
        router: Arc<hya_provider::ProviderRouter>,
    ) -> Self {
        Self { categories, router }
    }
}
/// Immutable ownership and persistence metadata for one Workflow route activation.
pub(crate) struct WorkflowTurnRouteSpec {
    pub(crate) route: WorkflowModelRoute,
    pub(crate) session: SessionId,
    pub(crate) run: Option<WorkflowRunId>,
    pub(crate) stage: String,
    pub(crate) member: MemberId,
    pub(crate) role: WorkflowMemberRole,
    pub(crate) iteration: u32,
    pub(crate) recorder: Option<WorkflowRouteRecorder>,
}

/// One acknowledged route outcome waiting for root-session persistence.
pub(crate) struct WorkflowRouteRecord {
    pub(crate) event: Event,
    pub(crate) ack: oneshot::Sender<Result<(), String>>,
}

/// Capacity-one route outcome handoff used by one explicit Workflow activation.
#[derive(Clone)]
pub(crate) struct WorkflowRouteRecorder {
    sender: mpsc::Sender<WorkflowRouteRecord>,
}

impl WorkflowRouteRecorder {
    /// Wrap the activation's capacity-one persistence channel.
    pub(crate) fn new(sender: mpsc::Sender<WorkflowRouteRecord>) -> Self {
        Self { sender }
    }

    /// Send one outcome and wait until the owning root persists it.
    pub(crate) async fn record(&self, event: Event) -> Result<(), crate::error::CoreError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.sender
            .send(WorkflowRouteRecord { event, ack: ack_tx })
            .await
            .map_err(|_| {
                crate::error::CoreError::Invalid(
                    "Workflow route outcome persistence unavailable".to_string(),
                )
            })?;
        let result = ack_rx.await.map_err(|_| {
            crate::error::CoreError::Invalid(
                "Workflow route outcome persistence acknowledgement lost".to_string(),
            )
        })?;
        result.map_err(crate::error::CoreError::Invalid)
    }
}

#[derive(Default)]
struct WorkflowRouteAttempt {
    step: u32,
    candidate_index: Option<usize>,
    pending_failure: Option<hya_proto::WorkflowRouteFailureClass>,
}

/// Request-local route metadata and exactly-once finalization guard.
pub(crate) struct WorkflowTurnRoute {
    route: WorkflowModelRoute,
    session: SessionId,
    run: Option<WorkflowRunId>,
    stage: String,
    member: MemberId,
    role: WorkflowMemberRole,
    iteration: u32,
    recorder: Option<WorkflowRouteRecorder>,
    attempt: Arc<Mutex<WorkflowRouteAttempt>>,
}

impl Clone for WorkflowTurnRoute {
    fn clone(&self) -> Self {
        Self {
            route: self.route.clone(),
            session: self.session,
            run: self.run,
            stage: self.stage.clone(),
            member: self.member,
            role: self.role,
            iteration: self.iteration,
            recorder: self.recorder.clone(),
            attempt: Arc::clone(&self.attempt),
        }
    }
}

impl WorkflowTurnRoute {
    /// Build route metadata for one assistant/provider stream-group owner.
    pub(crate) fn new(spec: WorkflowTurnRouteSpec) -> Self {
        Self {
            route: spec.route,
            session: spec.session,
            run: spec.run,
            stage: spec.stage,
            member: spec.member,
            role: spec.role,
            iteration: spec.iteration,
            recorder: spec.recorder,
            attempt: Arc::new(Mutex::new(WorkflowRouteAttempt::default())),
        }
    }

    /// Resolve the immutable declaration-order route.
    pub(crate) fn route(&self) -> &WorkflowModelRoute {
        &self.route
    }

    /// Mark the candidate attempt before awaiting provider transport.
    pub(crate) fn begin_attempt(&self, candidate_index: usize) {
        let mut attempt = self
            .attempt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if attempt.candidate_index.is_none() {
            attempt.pending_failure = None;
        }
        attempt.candidate_index = Some(candidate_index);
    }

    /// Record the stable class that caused a forward pre-stream advance.
    pub(crate) fn record_failure(
        &self,
        candidate_index: usize,
        failure: hya_proto::WorkflowRouteFailureClass,
    ) {
        let mut attempt = self
            .attempt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        attempt.candidate_index = Some(candidate_index);
        attempt.pending_failure = Some(failure);
    }

    /// Mark a returned stream and retain the class that caused its final advance.
    pub(crate) fn selected(
        &self,
        candidate_index: usize,
        pending_failure: Option<hya_proto::WorkflowRouteFailureClass>,
    ) {
        let mut attempt = self
            .attempt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        attempt.candidate_index = Some(candidate_index);
        attempt.pending_failure = pending_failure;
    }

    /// Finalize at most one bounded outcome for the current stream group.
    pub(crate) async fn finalize(
        &self,
        failure: Option<hya_proto::WorkflowRouteFailureClass>,
    ) -> Result<(), crate::error::CoreError> {
        let (step, candidate_index, pending_failure) = {
            let mut attempt = self
                .attempt
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(candidate_index) = attempt.candidate_index.take() else {
                return Ok(());
            };
            let step = attempt.step;
            attempt.step = attempt.step.saturating_add(1);
            let pending_failure = attempt.pending_failure.take();
            (step, candidate_index, pending_failure)
        };
        let Some(candidate) = self.route.candidates.get(candidate_index) else {
            return Err(crate::error::CoreError::Invalid(
                "Workflow route selected candidate is out of bounds".to_string(),
            ));
        };
        let Some(run) = self.run else {
            return Ok(());
        };
        let Some(recorder) = self.recorder.as_ref() else {
            return Ok(());
        };
        let candidate_index = u32::try_from(candidate_index).unwrap_or(u32::MAX);
        recorder
            .record(Event::WorkflowStageRouteOutcome {
                session: self.session,
                run,
                stage: self.stage.clone(),
                member: self.member,
                role: self.role,
                iteration: self.iteration,
                step,
                candidate_index,
                model: candidate.model.clone(),
                reasoning: candidate.reasoning.as_str().to_string(),
                failure_class: failure
                    .or(pending_failure)
                    .unwrap_or(hya_proto::WorkflowRouteFailureClass::None),
            })
            .await
    }
}

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
                | hya_proto::Event::WorkflowStageRouteOutcome { .. }
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
