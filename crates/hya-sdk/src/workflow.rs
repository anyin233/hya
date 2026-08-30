//! Typed Workflow command, result, and replay-state mirrors.
//!
//! `hya-sdk` deliberately does not depend on `hya-proto` at runtime. These
//! serde-compatible mirrors keep the SDK transport-independent while retaining
//! the shared wire contract used by the server and in-process transports.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize, Serializer};

/// Stable catalog origin for a compiled Workflow source.
///
/// The server currently serializes this identity as an opaque string. Keeping
/// it as a string matches the SDK's existing session/team mirror types and
/// preserves future source formats without a client-side parser.
pub type WorkflowSourceId = String;

/// Canonical SHA-256 revision of normalized Workflow semantics.
///
/// Revisions are serialized as their canonical hexadecimal text.
pub type WorkflowRevision = String;

/// Durable Workflow run identity as serialized by the server.
pub type WorkflowRunId = String;

/// Runtime owner identity that admitted a Workflow run.
pub type OwnerRunId = String;

/// Canonical member identity in a parent Session's member projection.
pub type MemberId = String;
/// Base model candidate in an authored ordered fallback chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowModelCandidate {
    /// Stable provider/model identity without a Workflow variant suffix.
    pub id: String,
    /// Optional authored reasoning effort label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

/// One authored model assignment with declaration-order fallback candidates.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowModelAssignment {
    /// Preferred base model identity.
    pub id: String,
    /// Optional preferred reasoning effort label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Ordered fallback candidates tried after the preferred model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback: Vec<WorkflowModelCandidate>,
}

/// One admission-resolved candidate in a Stage route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowModelResolvedCandidate {
    /// Declaration-order index in the complete route.
    pub index: u32,
    /// Base model identity without a variant suffix.
    pub id: String,
    /// Required canonical effort label (`none` means Off).
    pub reasoning: String,
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
    #[serde(serialize_with = "serialize_session_id")]
    pub session: String,
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
    pub model: String,
    /// Required canonical effort label (`none` means Off).
    pub reasoning: String,
    /// Stable provider/activation failure class.
    pub failure_class: WorkflowRouteFailureClass,
}
fn serialize_session_id<S>(value: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&canonical_session_id(value))
}

fn canonical_session_id(value: &str) -> String {
    let bytes = value.as_bytes();
    let hyphens = [8, 13, 18, 23];
    if bytes.len() != 36
        || hyphens.iter().any(|&index| bytes[index] != b'-')
        || bytes
            .iter()
            .enumerate()
            .any(|(index, &byte)| !hyphens.contains(&index) && !byte.is_ascii_hexdigit())
    {
        return value.to_string();
    }
    let mut canonical = String::with_capacity(36);
    canonical.push_str("ses_");
    for &byte in bytes {
        if byte != b'-' {
            canonical.push(char::from(byte.to_ascii_lowercase()));
        }
    }
    canonical
}

/// One Stage in a compiled Workflow info response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowStageInfo {
    /// Compiled Stage id.
    pub id: String,
    /// Optional author title.
    #[serde(default)]
    pub title: Option<String>,
    /// Target Agent id.
    pub agent: String,
    /// Zero-based topological level.
    pub level: usize,
    /// Direct predecessor ids in compiled join order.
    pub predecessors: Vec<String>,
    /// Optional resident actor key.
    #[serde(default)]
    pub actor: Option<String>,
    /// `once` or `loop`.
    pub mode: String,
    /// Optional authored worker model assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_model: Option<WorkflowModelAssignment>,
    /// Optional authored verifier model assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_model: Option<WorkflowModelAssignment>,
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

/// One Workflow discovery row. Invalid sources remain visible with an explicit error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSummary {
    /// Declared name, or source stem when compilation failed.
    pub name: String,
    /// Description when compilation succeeded.
    pub description: String,
    /// Stable source identity when compilation succeeded.
    #[serde(default)]
    pub source: Option<WorkflowSourceId>,
    /// Canonical revision when compilation succeeded.
    #[serde(default)]
    pub revision: Option<WorkflowRevision>,
    /// Compiled Stage ids, or empty for an invalid source.
    pub stages: Vec<String>,
    /// Source path for diagnostics.
    pub path: String,
    /// Compiler failure for an invalid source.
    #[serde(default)]
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
    /// Discovery rows from [`WorkflowCommand::List`].
    List {
        /// Discovered source rows.
        workflows: Vec<WorkflowSummary>,
    },
    /// Compiled graph from [`WorkflowCommand::Info`].
    Info {
        /// Exact compiled metadata.
        workflow: WorkflowInfo,
    },
    /// Replay-derived state after [`WorkflowCommand::Select`].
    Selected {
        /// Current Workflow projection.
        state: WorkflowProjection,
    },
    /// Replay-derived state from [`WorkflowCommand::State`].
    State {
        /// Current Workflow projection.
        state: WorkflowProjection,
    },
    /// Run admission or terminal state from [`WorkflowCommand::Run`].
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
    pub agent: String,
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

/// Role of one canonical member reference within a Workflow Stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowMemberRole {
    /// Worker or resident actor activation.
    Worker,
    /// Independent loop verifier judgment.
    Verifier,
}

/// One canonical member reference linked to a Workflow Stage.
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
    /// Canonical member references in event order.
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
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn command_and_result_use_shared_tagged_wire_shapes() {
        let command = WorkflowCommand::Run {
            name: Some("demo".to_owned()),
            expected_revision: Some("ab".repeat(32)),
            inputs: BTreeMap::from([(String::from("topic"), String::from("engines"))]),
            run: Some("run-1".to_owned()),
        };
        let encoded = serde_json::to_value(&command).expect("encode command");
        assert_eq!(encoded["command"], "run");
        assert_eq!(encoded["expected_revision"], "ab".repeat(32));
        assert_eq!(encoded["inputs"]["topic"], "engines");

        let result: WorkflowCommandResult = serde_json::from_value(serde_json::json!({
            "kind": "state",
            "state": { "selection": null, "run": null }
        }))
        .expect("decode state result");
        assert!(matches!(result, WorkflowCommandResult::State { state } if state.run.is_none()));
    }

    #[test]
    fn workflow_state_round_trips_uuid_and_revision_text_unchanged() {
        let state: WorkflowProjection = serde_json::from_value(serde_json::json!({
            "selection": {
                "source": "bundle:demo",
                "name": "demo",
                "revision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            },
            "run": {
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "workflow": {
                    "source": "bundle:demo",
                    "name": "demo",
                    "revision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                },
                "request_hash": "hash",
                "owner": "550e8400-e29b-41d4-a716-446655440001",
                "status": "running",
                "stages": [],
                "error": null
            }
        }))
        .expect("decode workflow state");
        let encoded = serde_json::to_value(state).expect("encode workflow state");
        assert_eq!(encoded["selection"]["source"], "bundle:demo");
        assert_eq!(encoded["run"]["id"], "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(
            encoded["run"]["workflow"]["revision"],
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
    }
}
