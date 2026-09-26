//! `hya-proto` — wire types shared by client + server.
//!
//! Two invariants (design.md §3): tagged enums everywhere (never `untagged`) and
//! a newtype per id. This crate is dependency-light (serde / uuid only) so the
//! TUI can share types without pulling sqlx/tokio into its build graph.

/// Native HTTP request/response DTOs for session create, prompt, shell, and event queries.
pub mod api;
/// Canonical streaming `Event` enum and ordered `Envelope` unit for the log and bus.
pub mod event;
/// Strong id newtypes (`SessionId`, message/part/tool ids, claim epochs, etc.).
pub mod ids;
/// Mail address and kind types for event-sourced team messaging (ADR-0001).
pub mod mail;
/// Model-facing `Message` / `Part` / finish and usage value types.
pub mod message;
/// Agent, model, and tool name newtypes plus the model-facing tool schema.
pub mod model;
/// Idempotent event-log reducer: `Projection` and session/team view structs.
pub mod projection;
/// Pure run-tree assembler over reduced session projections (no I/O).
pub mod projection_tree;
/// File snapshot and revert value types (`FilesChanged`, `SessionReverted`).
pub mod revert;
/// Canonical agent paths, the parent/sibling/report scope rule, and
/// unit-qualified channel keys (task 08-07).
pub mod scope;
/// Session todo rows (`TodosUpdated` event and projection).
pub mod todo;
/// Wire types for token accounting (mode and count provenance).
pub mod tokens;
/// Session-level billed-usage fold keyed by serving model and purpose.
pub mod usage;
/// Durable Workflow selection, run, Stage, and member projection types.
pub mod workflow;
/// Compat workspace-adapter list entry.
pub mod workspace;

pub use event::{
    ArchiveReason, CompactionStrategy, Envelope, Event, ReportOutcome, WorkflowRouteFailureClass,
    WorkflowStageRouteOutcome,
};
pub use ids::{
    ActorClaim, ActorEpoch, ConfigGeneration, EventSeq, GoalId, LoopRunId, MemberId, MessageId,
    OperationId, OwnerRunId, PartId, PermissionRequestId, QuestionRequestId, SessionId, TeamRunId,
    ToolCallId, WorkflowRunId,
};
pub use mail::{
    CHANNEL_RANDOM_LEN, ChannelKind, MailEndpoint, MailKind, is_minted_channel_id, mint_channel_id,
};
pub use message::{
    FinishCause, FinishReason, MemberRunStatus, Message, Part, Role, RosterStatus, SubagentMode,
    TokenUsage, ToolPartState, UsagePurpose,
};
pub use model::{AgentName, ModelRef, ToolName, ToolSchema};
pub use projection::{
    ArchivedEntry, ChannelProjection, ChannelResolveError, ContextStatusProjection,
    HandoffProjection, MailMessage, MemberProjection, MessageError, MessageProjection,
    PROJECTION_REDUCER_VERSION, PartProjection, Projection, ResidentWorkProjection, RosterEntry,
    ScopedRoster, SessionArchiveError, SessionProjection, TeamProjection, session_archive_event,
};
pub use projection_tree::{RunTreeNode, build_run_tree};
pub use revert::{FileChange, FileChangeRecord, FileRestore, FileState, RevertProjection};
pub use scope::{ANNOUNCE_CHANNEL, HARNESS_HANDLE, ROOT_HANDLE, Relation, in_scope, relation};
pub use todo::{TodoItem, TodoStatus};
pub use usage::{MessageUsage, OutputSplit, SessionUsage, UNATTRIBUTED_MODEL, UsageTotals};
pub use workflow::{
    WorkflowAvailability, WorkflowCommand, WorkflowCommandResult, WorkflowDelivery,
    WorkflowIdentity, WorkflowInfo, WorkflowMemberProjection, WorkflowMemberRole,
    WorkflowModelAssignment, WorkflowModelCandidate, WorkflowModelResolvedCandidate,
    WorkflowProjection, WorkflowRevision, WorkflowRevisionParseError, WorkflowRunProjection,
    WorkflowRunResult, WorkflowRunStatus, WorkflowSourceId, WorkflowStageInfo, WorkflowStagePlan,
    WorkflowStageProjection, WorkflowStageStatus, WorkflowSummary,
};
pub use workspace::WorkspaceAdapterInfo;

/// Unix-epoch milliseconds. Used for `Envelope.ts_millis` and DB timestamps.
#[must_use]
pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}
