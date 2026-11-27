//! User-authored workflow plane for the `workflow` tool.
//!
//! The tool plane only frames requests. Execution stays in the application
//! layer, where the engine, turn binding, and caller authorization are
//! available. Hosts route every command through the app-owned
//! `WorkflowControl::execute` seam.

use std::collections::{BTreeMap, BTreeSet};

use hya_proto::{
    AgentName, MemberId, ModelRef, OwnerRunId, SessionId, WorkflowAvailability, WorkflowIdentity,
    WorkflowInfo, WorkflowModelAssignment, WorkflowModelResolvedCandidate, WorkflowRevision,
    WorkflowRunId, WorkflowSummary,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::tool::{ToolError, ToolOperation};

/// Delivery contract requested by a Workflow caller.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowDelivery {
    /// Return after durable admission; progress arrives in events.
    Started,
    /// Await the terminal run projection before returning.
    #[default]
    Finished,
}

/// One command accepted by the app-owned Workflow control seam.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum WorkflowCommand {
    /// List every discovered Workflow source.
    List,
    /// Return one exact compiled Workflow graph.
    Info {
        /// Declared Workflow name.
        name: String,
    },
    /// Persist one selected source/revision identity.
    Select {
        /// Declared Workflow name.
        name: String,
        /// Optional optimistic compiler revision.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_revision: Option<WorkflowRevision>,
    },
    /// Return replay-derived state for one Session.
    State,
    /// Admit and optionally execute one named or selected Workflow.
    Run {
        /// Explicit name, or `None` to use the durable selection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Optional optimistic compiler revision.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_revision: Option<WorkflowRevision>,
        /// Exact values for every declared Workflow input.
        #[serde(default)]
        inputs: BTreeMap<String, String>,
        /// Stable direct-call id for idempotent retries.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<WorkflowRunId>,
    },
}

/// Typed result returned by the app-owned Workflow control seam.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowCommandResult {
    /// Discovery rows from `List`.
    List {
        /// Discovered source rows.
        workflows: Vec<WorkflowSummary>,
    },
    /// Compiled graph from `Info`.
    Info {
        /// Exact compiled metadata.
        workflow: WorkflowInfo,
    },
    /// Replay-derived state after `Select`.
    Selected {
        /// Current Workflow projection.
        state: WorkflowProjection,
    },
    /// Replay-derived state from `State`.
    State {
        /// Current Workflow projection.
        state: WorkflowProjection,
    },
    /// Run admission or terminal state from `Run`.
    Run {
        /// Current or terminal run projection.
        result: WorkflowRunResult,
    },
}

/// Result of a new or idempotently replayed Workflow run request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowRunResult {
    /// Replay-derived current or historical run state.
    pub run: WorkflowRunProjection,
    /// True when the run had already been admitted and no work ran again.
    pub replayed: bool,
}

/// Terminal and active states of one Workflow run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRunStatus {
    /// The run can still admit or complete Stages.
    Running,
    /// Every required Stage completed.
    Completed,
    /// At least one Stage failed.
    Failed,
    /// The current owner cancelled the run.
    Cancelled,
    /// Startup proved that the prior runtime owner exited.
    Interrupted,
}

impl WorkflowRunStatus {
    /// Return whether later run-terminal events must be ignored.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// Terminal and active states of one Workflow Stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStageStatus {
    /// The Stage is in the compiled plan but has not started.
    Pending,
    /// The Stage owns admitted work.
    Running,
    /// The Stage completed successfully.
    Completed,
    /// The Stage completed with a failure.
    Failed,
    /// Cancellation stopped the Stage.
    Cancelled,
    /// The run terminalized before this Stage started.
    Skipped,
}

impl WorkflowStageStatus {
    /// Return whether later Stage lifecycle events must not change this status.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }
}

/// Stable display/provenance data captured at run admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStagePlan {
    /// Compiled Stage id.
    pub id: String,
    /// Optional author title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Resolved target Agent id.
    pub agent: AgentName,
    /// `once` or `loop`.
    pub mode: String,
    /// Zero-based topological level.
    pub level: usize,
    /// Optional authored worker model assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_model: Option<WorkflowModelAssignment>,
    /// Admission-selected worker candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_worker_model: Option<WorkflowModelResolvedCandidate>,
    /// Optional authored verifier model assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_model: Option<WorkflowModelAssignment>,
    /// Admission-selected verifier candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_verifier_model: Option<WorkflowModelResolvedCandidate>,
}

/// Role of one canonical Member reference within a Workflow Stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowMemberRole {
    /// Worker or resident actor activation.
    Worker,
    /// Independent loop verifier judgment.
    Verifier,
}

/// One canonical Member reference linked to a Workflow Stage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowMemberProjection {
    /// Member identity in the Session tree.
    pub member: MemberId,
    /// Worker or independent verifier role.
    pub role: WorkflowMemberRole,
    /// Zero-based activation iteration.
    pub iteration: u32,
}

/// Stable, bounded classification for one Workflow route outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRouteFailureClass {
    /// The selected candidate succeeded without a fallback advance.
    None,
    /// Retryable transport failure before a stream was established.
    Transport,
    /// Upstream rate limiting before a stream was established.
    RateLimited,
    /// Retryable upstream server failure before a stream was established.
    Server,
    /// No provider route claimed the candidate model.
    UnknownModel,
    /// Authentication or authorization failure.
    Auth,
    /// Provider capability or request incompatibility.
    Incompatible,
    /// Non-retryable HTTP/provider response failure.
    Http,
    /// Malformed or truncated provider stream.
    Decode,
    /// Internal invariant, store, or claim failure after an attempt started.
    Internal,
    /// Sidecar loss or task abort after an attempt started.
    Aborted,
    /// Explicit Workflow-owned activation cancellation.
    Cancelled,
}

/// Durable fields for one finalized explicit Workflow route stream group.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStageRouteOutcome {
    /// Owning root Session log.
    pub session: SessionId,
    /// Workflow run containing the Stage.
    pub run: WorkflowRunId,
    /// Compiled Stage id.
    pub stage: String,
    /// Canonical Session member reference.
    pub member: MemberId,
    /// Worker or independent verifier route.
    pub role: WorkflowMemberRole,
    /// Zero-based loop activation iteration.
    pub iteration: u32,
    /// Assistant/provider stream-group index.
    pub step: u32,
    /// Declaration-order candidate index selected or finally attempted.
    pub candidate_index: u32,
    /// Base model identity without a Workflow variant suffix.
    pub model: ModelRef,
    /// Required canonical effort label (`none` means Off).
    pub reasoning: String,
    /// Stable provider/activation failure class.
    pub failure_class: WorkflowRouteFailureClass,
}

/// Folded state of one Stage in the newest Workflow run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStageProjection {
    /// Stable plan metadata captured by run start.
    #[serde(flatten)]
    pub plan: WorkflowStagePlan,
    /// Latest monotonic Stage state.
    pub status: WorkflowStageStatus,
    /// Canonical Member references in event order.
    #[serde(default)]
    pub members: Vec<WorkflowMemberProjection>,
    /// Canonical finalized route outcomes in append order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route_outcomes: Vec<WorkflowStageRouteOutcome>,
}

/// Folded state of the newest Workflow run in one Session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowRunProjection {
    /// Durable run identity.
    pub id: WorkflowRunId,
    /// Exact source/revision identity executed by this run.
    pub workflow: WorkflowIdentity,
    /// Canonical hash of source, caller, inputs, and bound runtime semantics.
    pub request_hash: String,
    /// Runtime owner that admitted this run.
    pub owner: OwnerRunId,
    /// Latest monotonic run state.
    pub status: WorkflowRunStatus,
    /// Declaration-ordered Stage projections.
    pub stages: Vec<WorkflowStageProjection>,
    /// Bounded terminal error, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Replay-only Workflow state attached to a Session without changing transcript rows.
#[derive(Clone, Debug, Default, Eq, Serialize, Deserialize)]
pub struct WorkflowProjection {
    /// Current selected compiled identity, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<WorkflowIdentity>,
    /// Newest run, active or terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<WorkflowRunProjection>,
    /// Current runtime catalog status for the selected identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<WorkflowAvailability>,
    /// Run ids already admitted during this replay; not compared in `PartialEq`.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub seen_runs: BTreeSet<WorkflowRunId>,
}

impl PartialEq for WorkflowProjection {
    fn eq(&self, other: &Self) -> bool {
        self.selection == other.selection
            && self.run == other.run
            && self.availability == other.availability
    }
}

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
            .map_err(|error| {
                if error.code == "WORKFLOW_OVERLOADED" {
                    ToolError::Overloaded(error.message)
                } else {
                    ToolError::WorkflowControl {
                        code: error.code,
                        message: error.message,
                    }
                }
            })
    }
}

struct ClosedSink;

impl WorkflowRequestSink for ClosedSink {
    fn try_send(&self, _request: WorkflowRequest) -> Result<(), WorkflowSendError> {
        Err(WorkflowSendError::Closed)
    }
}
