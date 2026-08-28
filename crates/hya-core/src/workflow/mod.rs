//! User-authored workflow DAGs composed over the bounded subagent primitives.
//!
//! A workflow is one user-authored file describing a DAG of stages
//! ([`WorkflowDef`]). [`plan`] levelizes the graph into parallel batches;
//! [`run`](self) executes each batch through [`crate::subagent`]'s governed team
//! path (`pre_admit_team` / `run_pre_admitted_team`) so user composition can
//! never bypass depth, concurrency, or per-run spawn budgets. Fan-in is explicit:
//! consuming-stage templates reference upstream bounded outputs with
//! `{{stage_id}}` placeholders. hya ships **zero** built-in workflows; users opt
//! in by authoring files under `<workdir>/.hya/workflows` (see [`parse`]).

mod model;
mod parse;
mod plan;
mod run;

use thiserror::Error;

pub use model::{FailurePolicy, StageDef, StageMode, VerifySpec, WorkflowDef};
pub use parse::{
    discover_workflow_files, load_workflow_by_name, load_workflow_file, workflow_dirs_for_workdir,
};
pub use plan::{WorkflowPlan, build_plan};
pub use run::{
    MAX_STAGE_OUTPUT_CHARS, StageReport, StageStatus, WorkflowRunContext, WorkflowRunReport,
    WorkflowStatus, run_workflow,
};

/// Failures surfaced by workflow parsing, planning, or execution.
#[derive(Debug, Error)]
pub enum WorkflowError {
    /// The file could not be parsed as a workflow definition.
    #[error("workflow parse: {0}")]
    Parse(String),
    /// The definition parsed but violates structural or graph rules.
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
