//! User-authored workflow plane for the `workflow` tool.
//!
//! The tool plane only frames requests. Execution stays in the application
//! layer, where the engine, turn binding, and caller authorization are
//! available. Hosts route every command through the app-owned
//! `WorkflowControl::execute` seam.

use std::collections::BTreeMap;

use async_trait::async_trait;
use hya_proto::{
    SessionId, ToolSchema, WorkflowCommand, WorkflowCommandResult, WorkflowRevision, WorkflowRunId,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError, ToolOperation, obj_schema};

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

    async fn execute(
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

#[derive(Deserialize)]
struct WorkflowToolInput {
    /// Command name. `list` remains the default for compatibility.
    #[serde(default, alias = "command")]
    action: Option<String>,
    /// Declared Workflow name for `info`, `select`, and `run`.
    #[serde(default)]
    name: Option<String>,
    /// Optimistic compiler revision for `select` and `run`.
    #[serde(default)]
    expected_revision: Option<WorkflowRevision>,
    /// Values for the Workflow's declared inputs (`run` only).
    #[serde(default)]
    inputs: BTreeMap<String, Value>,
    /// Stable direct-call run id for idempotent `run` retries.
    #[serde(default)]
    run: Option<WorkflowRunId>,
}

/// Execute one typed Workflow command through the application control seam.
pub struct WorkflowTool;

#[async_trait]
impl Tool for WorkflowTool {
    fn name(&self) -> &str {
        "workflow"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "workflow",
            "List, inspect, select, run, or inspect the state of user-authored workflow DAGs. Workflow execution uses the same durable Session control path as the CLI and direct server requests.",
            json!({
                "action": {
                    "type": "string",
                    "enum": ["list", "info", "select", "run", "state"],
                    "description": "list: discover workflows (default); info: inspect one graph; select: persist one workflow selection; run: execute one workflow; state: read durable selection/run state"
                },
                "name": {
                    "type": "string",
                    "description": "Declared Workflow name (required for action=info|select|run)"
                },
                "expected_revision": {
                    "type": "string",
                    "description": "Optional 64-character compiler revision fence for select/run"
                },
                "inputs": {
                    "type": "object",
                    "description": "Values for the Workflow's declared input keys (action=run)",
                    "additionalProperties": { "type": ["string", "number", "boolean"] }
                },
                "run": {
                    "type": "string",
                    "description": "Optional stable Workflow run id for direct idempotent retries"
                }
            }),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: WorkflowToolInput =
            serde_json::from_value(input).map_err(|error| ToolError::Input(error.to_string()))?;
        let action = input.action.as_deref().unwrap_or("list");
        let command = match action {
            "list" => WorkflowCommand::List,
            "info" => WorkflowCommand::Info {
                name: required_name(input.name, "info")?,
            },
            "select" => WorkflowCommand::Select {
                name: required_name(input.name, "select")?,
                expected_revision: input.expected_revision,
            },
            "state" => WorkflowCommand::State,
            "run" => {
                let name = input.name.map(|name| name.trim().to_string());
                if name.as_deref().is_some_and(str::is_empty) {
                    return Err(ToolError::Input(
                        "action=run `name` must not be empty".to_string(),
                    ));
                }
                ctx.permission
                    .assert(
                        Action::Task,
                        Resource::Subagent(format!(
                            "workflow:{}",
                            name.as_deref().unwrap_or("selected")
                        )),
                    )
                    .await?;
                let inputs = input
                    .inputs
                    .into_iter()
                    .map(|(key, value)| {
                        let value = match value {
                            Value::String(text) => text,
                            other => other.to_string(),
                        };
                        (key, value)
                    })
                    .collect();
                WorkflowCommand::Run {
                    name,
                    expected_revision: input.expected_revision,
                    inputs,
                    run: input.run,
                }
            }
            other => {
                return Err(ToolError::Input(format!(
                    "unknown workflow action `{other}` (expected list|info|select|run|state)"
                )));
            }
        };
        let result = ctx
            .workflows
            .execute(ctx.operation, command, ctx.cancel.clone())
            .await?;
        serde_json::to_value(result)
            .map_err(|error| ToolError::Other(format!("serialize workflow result: {error}")))
    }
}

fn required_name(name: Option<String>, action: &str) -> Result<String, ToolError> {
    let name = name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return Err(ToolError::Input(format!("action={action} requires `name`")));
    }
    Ok(name)
}
