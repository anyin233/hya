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
    StageReport, StageStatus, WorkflowRunContext, WorkflowRunReport, WorkflowStatus, run_workflow,
};
pub use source::{
    discover_workflow_files, load_workflow_by_name, load_workflow_file, workflow_dirs_for_workdir,
};

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
