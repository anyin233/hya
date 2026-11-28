//! Error types and stable error-code mappings for `hya/workflow`.

use hya_proto::SessionId;
use hya_runtime_abi::HostError;
use thiserror::Error;

use crate::compile::{WorkflowCompileError, WorkflowRenderError};
use crate::model::{WorkflowRevision, WorkflowRunId};

/// Failures surfaced by workflow discovery, compilation, preflight, or DAG execution.
#[derive(Debug, Error)]
pub enum WorkflowError {
    /// A Workflow source could not be read from disk.
    #[error("Workflow source `{source_name}`: {detail}")]
    Source {
        /// Path or source identity.
        source_name: String,
        /// Read failure detail.
        detail: String,
    },
    /// The shared compiler rejected a Workflow document.
    #[error(transparent)]
    Compile(#[from] WorkflowCompileError),
    /// Runtime values did not satisfy the compiled input/evidence contract.
    #[error(transparent)]
    Render(#[from] WorkflowRenderError),
    /// The compiled Workflow cannot run under the supplied runtime context.
    #[error("invalid workflow `{workflow}`: {detail}")]
    Invalid {
        /// Workflow name that failed validation.
        workflow: String,
        /// Human-readable rule violation.
        detail: String,
    },
    /// A Workflow member would exceed the team depth limit.
    #[error("workflow admission: max recursion depth reached")]
    MaxDepth,
    /// The team capacity budget was exceeded.
    #[error("workflow admission: capacity exceeded: {0}")]
    CapacityExceeded(String),
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
    /// Host runtime failure during preflight or execution.
    #[error(transparent)]
    Host(#[from] HostError),
}

/// Stable workflow control failures shared by tool, CLI, HTTP, and SDK surfaces.
#[derive(Debug, Error)]
pub enum WorkflowControlError {
    /// No compiled Workflow with the requested name exists in the Session catalog.
    #[error("Workflow `{name}` was not found")]
    NotFound {
        /// Requested declared name.
        name: String,
    },
    /// A discovered source did not compile.
    #[error("invalid Workflow source: {0}")]
    InvalidSource(String),
    /// The caller selected against an obsolete compiler revision.
    #[error("Workflow revision changed: expected {expected}, current {actual}")]
    StaleRevision {
        /// Revision supplied by the caller.
        expected: WorkflowRevision,
        /// Current compiler revision.
        actual: WorkflowRevision,
    },
    /// The Session does not exist or has no usable work directory.
    #[error("Session `{session}` was not found")]
    SessionNotFound {
        /// Requested Session.
        session: SessionId,
    },
    /// No Workflow has been selected for a selected-run request.
    #[error("Session `{session}` has no selected Workflow")]
    MissingSelection {
        /// Requested Session.
        session: SessionId,
    },
    /// Runtime inputs do not match the compiled Workflow declaration.
    #[error("invalid Workflow inputs: {0}")]
    InvalidInput(String),
    /// The caller cannot spawn one declared worker or verifier Agent.
    #[error("Workflow authorization failed: {0}")]
    Unauthorized(String),
    /// A different run still owns this Session.
    #[error("Session `{session}` already has running Workflow `{run}`")]
    Busy {
        /// Busy Session.
        session: SessionId,
        /// Active run identity.
        run: WorkflowRunId,
    },
    /// Runtime binding cannot provide a complete semantic fingerprint or is stopped.
    #[error("Workflow runtime semantics are unavailable")]
    RuntimeUnavailable,
    /// One stable run id was retried with different immutable request data.
    #[error("Workflow run `{run}` conflicts with its original request")]
    OperationConflict {
        /// Conflicting Workflow run.
        run: WorkflowRunId,
    },
    /// Team capacity budget was exceeded.
    #[error("Workflow overloaded: {0}")]
    Overloaded(String),
    /// Governed preflight or execution failed after catalog validation.
    #[error("Workflow execution failed: {0}")]
    Execution(String),
    /// Host runtime failure.
    #[error(transparent)]
    Host(#[from] HostError),
}

impl WorkflowControlError {
    /// Machine-stable error code for native, tool, and CLI adapters.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "WORKFLOW_NOT_FOUND",
            Self::InvalidSource(_) => "WORKFLOW_INVALID_SOURCE",
            Self::StaleRevision { .. } => "WORKFLOW_STALE_REVISION",
            Self::SessionNotFound { .. } => "SESSION_NOT_FOUND",
            Self::MissingSelection { .. } => "WORKFLOW_NOT_SELECTED",
            Self::InvalidInput(_) => "WORKFLOW_INVALID_INPUT",
            Self::Unauthorized(_) => "WORKFLOW_UNAUTHORIZED",
            Self::Busy { .. } => "WORKFLOW_BUSY",
            Self::RuntimeUnavailable => "WORKFLOW_RUNTIME_UNAVAILABLE",
            Self::OperationConflict { .. } => "WORKFLOW_OPERATION_CONFLICT",
            Self::Overloaded(_) => "WORKFLOW_OVERLOADED",
            Self::Execution(_) | Self::Host(_) => "WORKFLOW_INTERNAL",
        }
    }

    /// Canonical HTTP status code and v2 wire error code string.
    #[must_use]
    pub const fn http_status_and_v2_code(&self) -> (u16, &'static str) {
        match self {
            Self::SessionNotFound { .. } => (404, "session_not_found"),
            Self::InvalidInput(_) | Self::InvalidSource(_) => (400, "invalid_argument"),
            Self::NotFound { .. } | Self::MissingSelection { .. } => (404, "not_found"),
            Self::Unauthorized(_) => (403, "permission_denied"),
            Self::Busy { .. } | Self::OperationConflict { .. } | Self::StaleRevision { .. } => {
                (409, "failed_precondition")
            }
            Self::RuntimeUnavailable | Self::Overloaded(_) => (503, "unavailable"),
            Self::Execution(_) | Self::Host(_) => (500, "internal"),
        }
    }
}

/// Bound a persisted or public error string to 2,048 UTF-8 characters.
#[must_use]
pub fn bounded_error(value: &str) -> String {
    value.chars().take(2_048).collect()
}

/// Map failures detected before durable run admission to request categories.
#[must_use]
pub fn map_workflow_preflight_error(error: WorkflowError) -> WorkflowControlError {
    let detail = bounded_error(&error.to_string());
    match error {
        WorkflowError::Unauthorized { .. } => WorkflowControlError::Unauthorized(detail),
        WorkflowError::Render(_) => WorkflowControlError::InvalidInput(detail),
        WorkflowError::Source { .. }
        | WorkflowError::Compile(_)
        | WorkflowError::Invalid { .. } => WorkflowControlError::InvalidSource(detail),
        WorkflowError::CapacityExceeded(msg) => WorkflowControlError::Overloaded(msg),
        WorkflowError::Host(HostError::Unavailable(msg)) => WorkflowControlError::Overloaded(msg),
        WorkflowError::Host(error) => WorkflowControlError::Host(error),
        WorkflowError::MaxDepth => WorkflowControlError::Execution(detail),
    }
}

/// Convert a DAG execution failure while retaining a bounded rendered detail.
#[must_use]
pub fn map_workflow_error_with_detail(
    error: WorkflowError,
    detail: String,
) -> WorkflowControlError {
    match error {
        WorkflowError::Unauthorized { .. } => WorkflowControlError::Unauthorized(detail),
        WorkflowError::Render(_) => WorkflowControlError::InvalidInput(detail),
        WorkflowError::CapacityExceeded(msg) => WorkflowControlError::Overloaded(msg),
        WorkflowError::Host(HostError::Unavailable(msg)) => WorkflowControlError::Overloaded(msg),
        WorkflowError::Host(error) => WorkflowControlError::Host(error),
        WorkflowError::MaxDepth
        | WorkflowError::Source { .. }
        | WorkflowError::Compile(_)
        | WorkflowError::Invalid { .. } => WorkflowControlError::Execution(detail),
    }
}
