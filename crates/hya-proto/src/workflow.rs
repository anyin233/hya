//! Durable Workflow identities and replay-only Session projection types.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{AgentName, MemberId, OwnerRunId, WorkflowRunId};

/// Stable catalog origin for a compiled Workflow source.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkflowSourceId(String);

impl WorkflowSourceId {
    /// Construct a stable source identity from a catalog-owned value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the stable source identity.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkflowSourceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Canonical SHA-256 revision of normalized Workflow semantics.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkflowRevision([u8; 32]);

impl WorkflowRevision {
    /// Construct a revision from compiler digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Return canonical digest bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl std::fmt::Display for WorkflowRevision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for WorkflowRevision {
    type Err = WorkflowRevisionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 {
            return Err(WorkflowRevisionParseError(
                "Workflow revision must contain exactly 64 hexadecimal characters".to_string(),
            ));
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = hex_value(pair[0]).ok_or_else(|| {
                WorkflowRevisionParseError("Workflow revision contains non-hex data".to_string())
            })?;
            let low = hex_value(pair[1]).ok_or_else(|| {
                WorkflowRevisionParseError("Workflow revision contains non-hex data".to_string())
            })?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for WorkflowRevision {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WorkflowRevision {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Error returned by [`WorkflowRevision`] text parsing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowRevisionParseError(String);

impl std::fmt::Display for WorkflowRevisionParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WorkflowRevisionParseError {}

/// Decode one ASCII hexadecimal digit.
const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
/// One Stage in a compiled Workflow info response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStageInfo {
    /// Compiled Stage id.
    pub id: String,
    /// Optional author title.
    pub title: Option<String>,
    /// Target Agent id.
    pub agent: String,
    /// Zero-based topological level.
    pub level: usize,
    /// Direct predecessor ids in compiled join order.
    pub predecessors: Vec<String>,
    /// Optional resident actor key.
    pub actor: Option<String>,
    /// `once` or `loop`.
    pub mode: String,
}

/// Valid compiled Workflow metadata returned by an `info` command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowInfo {
    /// Stable source/name/revision identity.
    pub identity: WorkflowIdentity,
    /// Human-readable description.
    pub description: String,
    /// Required input names and descriptions.
    pub inputs: BTreeMap<String, String>,
    /// Failure policy wire value.
    pub on_failure: String,
    /// Compiled Stages in stable graph order.
    pub stages: Vec<WorkflowStageInfo>,
    /// Source path for diagnostics.
    pub path: String,
}

/// One discovery row. Invalid sources remain visible with an explicit error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSummary {
    /// Declared name, or source stem when compilation failed.
    pub name: String,
    /// Description when compilation succeeded.
    pub description: String,
    /// Stable source identity when compilation succeeded.
    pub source: Option<WorkflowSourceId>,
    /// Canonical revision when compilation succeeded.
    pub revision: Option<WorkflowRevision>,
    /// Compiled Stage ids, or empty for an invalid source.
    pub stages: Vec<String>,
    /// Source path for diagnostics.
    pub path: String,
    /// Compiler failure for an invalid source.
    pub error: Option<String>,
}

/// Result of a new or idempotently replayed Workflow run request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowRunResult {
    /// Replay-derived current or historical run state.
    pub run: WorkflowRunProjection,
    /// True when the run had already been admitted and no work ran again.
    pub replayed: bool,
}

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

/// Exact compiled Workflow identity selected or executed by a Session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowIdentity {
    /// Stable catalog source identity.
    pub source: WorkflowSourceId,
    /// Author-declared Workflow name.
    pub name: String,
    /// Canonical compiler revision.
    pub revision: WorkflowRevision,
}

/// Runtime catalog availability for a persisted Workflow identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowAvailability {
    /// The exact source and revision are present in the current catalog.
    Available,
    /// The exact source remains present but changed or no longer compiles.
    Stale,
    /// The exact source is absent from the current catalog.
    Unavailable,
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

/// Folded state of one Stage in the newest Workflow run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStageProjection {
    /// Stable plan metadata captured by run start.
    #[serde(flatten)]
    pub plan: WorkflowStagePlan,
    /// Latest monotonic Stage state.
    pub status: WorkflowStageStatus,
    /// Canonical Member references in event order.
    pub members: Vec<WorkflowMemberProjection>,
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
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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
    /// Run ids already admitted during this replay; not part of the wire state.
    #[serde(skip)]
    pub(crate) seen_runs: BTreeSet<WorkflowRunId>,
}
