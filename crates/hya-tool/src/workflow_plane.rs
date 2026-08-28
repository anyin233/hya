//! User-authored workflow plane for the `workflow` tool.
//!
//! Mirrors [`crate::spawn`]: the tool plane only frames requests; execution
//! lives host-side where the engine, binding, and caller authorization are
//! available. Hosts MUST route runs through the governed workflow executor
//! (`hya-core::workflow::run_workflow`, which goes through `pre_admit_team` /
//! `run_pre_admitted_team`) so user DAGs can never bypass subagent depth,
//! concurrency, or per-run budget caps. A disconnected plane surfaces the tool
//! as unavailable instead of pretending to run anything.

use std::collections::BTreeMap;

use async_trait::async_trait;
use hya_proto::{SessionId, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError, ToolOperation, obj_schema};

/// What the calling agent asked the host to do.
#[derive(Clone, Debug)]
pub enum WorkflowAction {
    /// Discover workflows under the session workdir's discovery roots.
    List,
    /// Execute one named user workflow with per-run inputs.
    Run {
        /// Workflow `name` as declared in its definition file.
        name: String,
        /// Values for every declared input key.
        inputs: BTreeMap<String, String>,
    },
}

/// One discovered workflow summary (list action).
#[derive(Clone, Debug, serde::Serialize)]
pub struct WorkflowSummary {
    /// Declared workflow name.
    pub name: String,
    /// Human description.
    pub description: String,
    /// Source file path (display form).
    pub path: String,
    /// Declared stage ids in declaration order; empty when invalid.
    pub stages: Vec<String>,
    /// Parse/validation error detail when the file could not be loaded.
    pub error: Option<String>,
}

/// One executed stage's bounded result (run action).
#[derive(Clone, Debug, serde::Serialize)]
pub struct WorkflowStageOutcome {
    /// Stage id from the definition.
    pub stage: String,
    /// Resolved stage agent id.
    pub agent: String,
    /// Terminal status (`done` | `failed`).
    pub status: String,
    /// Bounded final output (or failure summary).
    pub output: String,
}

/// Terminal result of one workflow run (run action).
#[derive(Clone, Debug, serde::Serialize)]
pub struct WorkflowOutcome {
    /// Overall terminal state (`completed` | `failed` | `cancelled`).
    pub status: String,
    /// Per-stage reports for stages that ran, declaration order.
    pub stages: Vec<WorkflowStageOutcome>,
}

/// Host response for one framed request.
pub type WorkflowReply = Result<WorkflowReplyPayload, String>;

/// Successful host response payload.
#[derive(Clone, Debug)]
pub enum WorkflowReplyPayload {
    /// Discovery results across all roots.
    List(Vec<WorkflowSummary>),
    /// One finished run.
    Run(WorkflowOutcome),
}

/// A framed workflow request awaiting host execution.
pub struct WorkflowRequest {
    /// Session whose turn issued the call; the lead lineage governs admission.
    pub parent: SessionId,
    /// Persisted operation identity for this tool call.
    pub operation: ToolOperation,
    /// Caller-reachable agent roster captured from the triggering turn.
    pub agents: std::sync::Arc<[crate::AgentDef]>,
    /// Requested action.
    pub action: WorkflowAction,
    /// Cooperative cancellation for long-running DAGs.
    pub cancel: CancellationToken,
    /// Oneshot back to the awaiting tool call.
    pub reply: oneshot::Sender<WorkflowReply>,
}

/// Sink side of the plane; hosts implement transport however they like.
pub trait WorkflowRequestSink: Send + Sync {
    /// Enqueue one request; `Full` means backpressure, `Closed` no runtime.
    ///
    /// # Errors
    /// [`WorkflowSendError`] when the queue is full or closed.
    fn try_send(&self, request: WorkflowRequest) -> Result<(), WorkflowSendError>;
}

/// Send-side failures mirrored after spawn-plane semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowSendError {
    /// The host queue is saturated.
    Full,
    /// The host runtime is gone or never wired.
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

/// Session-scoped handle the `workflow` tool executes through.
///
/// Cheap to clone; scope it per turn via [`Self::for_session_with_agents`].
#[derive(Clone)]
pub struct WorkflowPlane {
    sink: std::sync::Arc<dyn WorkflowRequestSink>,
    session: Option<SessionId>,
    agents: std::sync::Arc<[crate::AgentDef]>,
}

impl Default for WorkflowPlane {
    fn default() -> Self {
        Self::disconnected()
    }
}

impl WorkflowPlane {
    /// Disconnected plane for engines/tests without a workflow host.
    #[must_use]
    pub fn disconnected() -> Self {
        Self {
            sink: std::sync::Arc::new(ClosedSink),
            session: None,
            agents: std::sync::Arc::from([]),
        }
    }

    /// Plane over a custom sink (host wiring).
    #[must_use]
    pub fn from_sink(sink: std::sync::Arc<dyn WorkflowRequestSink>) -> Self {
        Self {
            sink,
            session: None,
            agents: std::sync::Arc::from([]),
        }
    }

    /// Bounded channel pair; capacity bounds queued (not running) requests.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> (Self, mpsc::Receiver<WorkflowRequest>) {
        let capacity = capacity.clamp(1, 65_535);
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self::from_sink(std::sync::Arc::new(ChannelWorkflowSink { tx })),
            rx,
        )
    }

    /// Scope to the calling session and its authorized roster.
    #[must_use]
    pub fn for_session_with_agents(
        &self,
        session: SessionId,
        agents: std::sync::Arc<[crate::AgentDef]>,
    ) -> Self {
        Self {
            agents,
            session: Some(session),
            ..self.clone()
        }
    }

    async fn execute(
        &self,
        operation: ToolOperation,
        action: WorkflowAction,
        cancel: CancellationToken,
    ) -> Result<WorkflowReplyPayload, ToolError> {
        let parent = self
            .session
            .ok_or_else(|| ToolError::Other("workflow tool requires a session".to_string()))?;
        let (tx, rx) = oneshot::channel();
        self.sink
            .try_send(WorkflowRequest {
                parent,
                operation,
                agents: self.agents.clone(),
                action,
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
            .map_err(ToolError::Other)
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
    /// `list` (default when omitted) or `run`.
    #[serde(default)]
    action: Option<String>,
    /// Required for `run`: the declared workflow name.
    #[serde(default)]
    name: Option<String>,
    /// Values for the workflow's declared inputs (`run` only).
    #[serde(default)]
    inputs: BTreeMap<String, Value>,
}

/// Launch user-assembled workflow DAGs mid-session.
///
/// Execution is governed exactly like the `task` tool's batches: the host runs
/// every stage batch through the shared pre-admitted team path, so max-depth,
/// streaming-concurrency, and per-run budgets apply unchanged.
pub struct WorkflowTool;

#[async_trait]
impl Tool for WorkflowTool {
    fn name(&self) -> &str {
        "workflow"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "workflow",
            "Discover and run user-authored workflow DAGs (files under .hya/workflows/). Each stage declares an agent plus a prompt template; stages sharing dependency levels fan out in parallel and downstream stages fan upstream outputs back in through {{stage_id}} placeholders. Runs are governed by the same subagent limits as the task tool.",
            json!({
                "action": {
                    "type": "string",
                    "enum": ["list", "run"],
                    "description": "list: summarize discovered workflows. run: execute one by name."
                },
                "name": {
                    "type": "string",
                    "description": "Workflow `name` from its definition file (required for action=run)"
                },
                "inputs": {
                    "type": "object",
                    "description": "Values for the workflow's declared input keys (action=run); every declared key must be provided",
                    "additionalProperties": { "type": ["string", "number", "boolean"] }
                }
            }),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: WorkflowToolInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let action_name = input.action.as_deref().unwrap_or("list");
        match action_name {
            "list" => {
                let payload = ctx
                    .workflows
                    .execute(ctx.operation, WorkflowAction::List, ctx.cancel.clone())
                    .await?;
                let WorkflowReplyPayload::List(summaries) = payload else {
                    return Err(ToolError::Other(
                        "workflow host returned an unexpected payload".to_string(),
                    ));
                };
                if summaries.is_empty() {
                    return Ok(json!({
                        "title": "workflows",
                        "metadata": { "count": 0 },
                        "output": "No user workflows discovered. Author YAML definitions \
                                   under <workdir>/.hya/workflows/ then re-run `workflow` \
                                   with action=list."
                    }));
                }
                let lines: Vec<String> = summaries
                    .iter()
                    .map(|summary| {
                        let base = format!(
                            "- {} — {} ({})",
                            summary.name, summary.description, summary.path
                        );
                        if let Some(error) = &summary.error {
                            format!("{base}\n  INVALID: {error}")
                        } else {
                            format!("{}\n  stages: {}", base, summary.stages.join(", "))
                        }
                    })
                    .collect();
                Ok(json!({
                    "title": format!("{} workflow(s)", summaries.len()),
                    "metadata": {
                        "names": summaries.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                    },
                    "output": lines.join("\n"),
                }))
            }
            "run" => {
                let name = input.name.unwrap_or_default().trim().to_string();
                if name.is_empty() {
                    return Err(ToolError::Input("action=run requires `name`".to_string()));
                }
                // Governed composition still counts as subagent work: reuse the
                // task permission class so existing ask/deny rules apply, scoped
                // to the workflow resource.
                ctx.permission
                    .assert(Action::Task, Resource::Subagent(format!("workflow:{name}")))
                    .await?;
                let mut inputs = BTreeMap::new();
                for (key, value) in input.inputs {
                    let rendered = match value {
                        Value::String(text) => text,
                        other => other.to_string(),
                    };
                    inputs.insert(key, rendered);
                }
                let payload = ctx
                    .workflows
                    .execute(
                        ctx.operation,
                        WorkflowAction::Run {
                            name: name.clone(),
                            inputs,
                        },
                        ctx.cancel.clone(),
                    )
                    .await?;
                let WorkflowReplyPayload::Run(outcome) = payload else {
                    return Err(ToolError::Other(
                        "workflow host returned an unexpected payload".to_string(),
                    ));
                };
                let failed =
                    outcome.stages.iter().any(|s| s.status != "done") || outcome.status == "failed";
                let rows: Vec<String> = outcome
                    .stages
                    .iter()
                    .map(|stage| {
                        format!(
                            "<stage id=\"{}\" agent=\"{}\" status=\"{}\">\n{}\n</stage>",
                            stage.stage, stage.agent, stage.status, stage.output
                        )
                    })
                    .collect();
                Ok(json!({
                    "title": format!("workflow {name}: {}", outcome.status),
                    "metadata": {
                        "status": outcome.status.clone(),
                        "failed": failed,
                        "stages": serde_json::to_value(&outcome.stages)
                            .unwrap_or(Value::Array(Vec::new())),
                    },
                    "output": format!(
                        "<workflow name=\"{}\" status=\"{}\">\n{}\n</workflow>",
                        name,
                        outcome.status,
                        rows.join("\n")
                    ),
                }))
            }
            other => Err(ToolError::Input(format!(
                "unknown workflow action `{other}` (expected list|run)"
            ))),
        }
    }
}
