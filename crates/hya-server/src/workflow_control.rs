//! Dependency-inverted Workflow control port for native and Compat HTTP routes.
//!
//! `hya-server` owns only this narrow port. The application runtime supplies an
//! implementation that delegates to `hya_app::WorkflowControl`; keeping the
//! port here avoids a dependency cycle while all HTTP surfaces share one
//! command/result contract.

use futures::future::BoxFuture;
use hya_proto::{
    SessionId, WorkflowCommand, WorkflowCommandResult, WorkflowDelivery, WorkflowProjection,
    WorkflowRunId,
};

/// Structured failure returned by the Workflow control port.
///
/// `code` is stable across HTTP, native, and SDK adapters. The server maps the
/// code to an HTTP status without inspecting application-owned error types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowControlError {
    /// Machine-readable stable error code.
    pub code: String,
    /// Bounded human-readable diagnostic.
    pub message: String,
}

impl WorkflowControlError {
    /// Create one structured control failure.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into().chars().take(128).collect(),
            message: message.into().chars().take(2_048).collect(),
        }
    }
}

impl std::fmt::Display for WorkflowControlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WorkflowControlError {}

/// Boxed asynchronous result returned by [`WorkflowControl::execute`].
pub type WorkflowControlFuture<'a> =
    BoxFuture<'a, Result<WorkflowCommandResult, WorkflowControlError>>;

/// Boxed asynchronous result returned by [`WorkflowControl::decorate`].
pub type WorkflowDecorationFuture<'a> =
    BoxFuture<'a, Result<WorkflowProjection, WorkflowControlError>>;

/// Server-owned asynchronous Workflow control port.
///
/// Implementations are cheap to clone behind `Arc` and safe for concurrent
/// native, legacy Compat, and Compat v2 requests. HTTP callers always pass
/// [`WorkflowDelivery::Started`], while non-HTTP callers may select the
/// completion-delivery policy supported by the application runtime.
pub trait WorkflowControl: Send + Sync {
    /// Execute one typed Workflow command for a Session.
    fn execute(
        &self,
        session: SessionId,
        command: WorkflowCommand,
        delivery: WorkflowDelivery,
    ) -> WorkflowControlFuture<'_>;

    /// Decorate replayed Workflow state with current runtime catalog data.
    ///
    /// The supplied projection is durable state. Implementations may only
    /// replace its derived availability field; all persisted fields must pass
    /// through unchanged.
    fn decorate(
        &self,
        _session: SessionId,
        state: WorkflowProjection,
    ) -> WorkflowDecorationFuture<'_> {
        Box::pin(async move { Ok(state) })
    }

    /// Return the active local Workflow run for Session exclusion.
    fn active_run(&self, _session: SessionId) -> Option<WorkflowRunId> {
        None
    }

    /// Request cooperative cancellation of one active Workflow run.
    fn cancel(&self, _session: SessionId) -> bool {
        false
    }
}

/// Default control port used by tests and callers that do not install a
/// runtime integration.
pub(crate) struct EmptyWorkflowControl;

impl WorkflowControl for EmptyWorkflowControl {
    fn execute(
        &self,
        _session: SessionId,
        _command: WorkflowCommand,
        _delivery: WorkflowDelivery,
    ) -> WorkflowControlFuture<'_> {
        Box::pin(async {
            Err(WorkflowControlError::new(
                "WORKFLOW_RUNTIME_UNAVAILABLE",
                "Workflow control is unavailable",
            ))
        })
    }

    fn decorate(
        &self,
        _session: SessionId,
        state: WorkflowProjection,
    ) -> WorkflowDecorationFuture<'_> {
        Box::pin(async move { Ok(state) })
    }
}
