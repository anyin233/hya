//! User-authored workflow plane for the `workflow` tool.
//!
//! The tool plane only frames requests. Execution stays in the application
//! layer, where the engine, turn binding, and caller authorization are
//! available. Hosts route every command through the app-owned
//! `WorkflowControl::execute` seam.

use hya_proto::{SessionId, WorkflowCommand, WorkflowCommandResult};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::tool::{ToolError, ToolOperation};

/// A framed workflow command awaiting application-owned execution.
pub struct WorkflowRequest {
    /// Session whose turn issued the call.
    pub parent: SessionId,
    /// Persisted identity of the model tool invocation.
    pub operation: ToolOperation,
    /// Typed command accepted by the app control seam.
    pub command: WorkflowCommand,
    /// Cooperative cancellation for the request.
    pub cancel: CancellationToken,
    /// Oneshot carrying the shared typed result or a bounded host error.
    pub reply: oneshot::Sender<Result<WorkflowCommandResult, WorkflowHostError>>,
}

/// Sink side of the workflow tool plane.
pub trait WorkflowRequestSink: Send + Sync {
    /// Enqueue one command; `Full` means backpressure and `Closed` means the
    /// application runtime is unavailable.
    ///
    /// # Errors
    /// Returns [`WorkflowSendError`] when the host cannot accept the request.
    fn try_send(&self, request: WorkflowRequest) -> Result<(), WorkflowSendError>;
}

/// Structured failure returned by the app-owned Workflow host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowHostError {
    /// Machine-stable Workflow control code.
    pub code: String,
    /// Bounded diagnostic safe for a tool result.
    pub message: String,
}

impl WorkflowHostError {
    /// Construct a bounded host error.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into().chars().take(128).collect(),
            message: message.into().chars().take(2_048).collect(),
        }
    }
}

/// Failure while framing a workflow command for the application runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowSendError {
    /// The host queue is saturated.
    Full,
    /// The host runtime is gone or was never wired.
    Closed,
}

struct ChannelWorkflowSink {
    tx: mpsc::Sender<WorkflowRequest>,
}

impl WorkflowRequestSink for ChannelWorkflowSink {
    fn try_send(&self, request: WorkflowRequest) -> Result<(), WorkflowSendError> {
        match self.tx.try_send(request) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => Err(WorkflowSendError::Full),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(WorkflowSendError::Closed),
        }
    }
}

/// Session-scoped handle used by the model `workflow` tool.
#[derive(Clone)]
pub struct WorkflowPlane {
    sink: std::sync::Arc<dyn WorkflowRequestSink>,
    session: Option<SessionId>,
}

impl Default for WorkflowPlane {
    fn default() -> Self {
        Self::disconnected()
    }
}

impl WorkflowPlane {
    /// Return a plane that fails closed because no workflow host is present.
    #[must_use]
    pub fn disconnected() -> Self {
        Self {
            sink: std::sync::Arc::new(ClosedSink),
            session: None,
        }
    }

    /// Build a plane over a host-provided request sink.
    #[must_use]
    pub fn from_sink(sink: std::sync::Arc<dyn WorkflowRequestSink>) -> Self {
        Self {
            sink,
            session: None,
        }
    }

    /// Build a bounded channel pair for application runtime wiring.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> (Self, mpsc::Receiver<WorkflowRequest>) {
        let capacity = capacity.clamp(1, 65_535);
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self::from_sink(std::sync::Arc::new(ChannelWorkflowSink { tx })),
            rx,
        )
    }

    /// Scope this plane to the Session issuing the model tool call.
    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Self {
        Self {
            session: Some(session),
            ..self.clone()
        }
    }

    /// Scope this plane to a Session while retaining the existing core call
    /// shape. Authorization is resolved from the bound application runtime;
    /// the caller roster is intentionally not copied into an adapter request.
    #[must_use]
    pub fn for_session_with_agents(
        &self,
        session: SessionId,
        _agents: std::sync::Arc<[crate::AgentDef]>,
    ) -> Self {
        self.for_session(session)
    }

    /// Send a typed workflow command to the application host.
    ///
    /// # Errors
    /// Returns a tool error when the host rejects or cannot accept the command.
    pub async fn execute(
        &self,
        operation: ToolOperation,
        command: WorkflowCommand,
        cancel: CancellationToken,
    ) -> Result<WorkflowCommandResult, ToolError> {
        let parent = self
            .session
            .ok_or_else(|| ToolError::Other("workflow tool requires a session".to_string()))?;
        let (tx, rx) = oneshot::channel();
        self.sink
            .try_send(WorkflowRequest {
                parent,
                operation,
                command,
                cancel,
                reply: tx,
            })
            .map_err(|error| match error {
                WorkflowSendError::Full => {
                    ToolError::Overloaded("workflow host queue is full; retry shortly".to_string())
                }
                WorkflowSendError::Closed => {
                    ToolError::Other("no workflow host is wired for this session".to_string())
                }
            })?;
        rx.await
            .map_err(|_| ToolError::Other("workflow host dropped the request".to_string()))?
            .map_err(|error| ToolError::WorkflowControl {
                code: error.code,
                message: error.message,
            })
    }
}

struct ClosedSink;

impl WorkflowRequestSink for ClosedSink {
    fn try_send(&self, _request: WorkflowRequest) -> Result<(), WorkflowSendError> {
        Err(WorkflowSendError::Closed)
    }
}
