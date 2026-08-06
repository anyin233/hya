//! `hya-proto` — wire types shared by client + server.
//!
//! Two invariants (design.md §3): tagged enums everywhere (never `untagged`) and
//! a newtype per id. This crate is dependency-light (serde / uuid only) so the
//! TUI can share types without pulling sqlx/tokio into its build graph.

// Fully documented; keep it that way. Removed when the workspace lint
// table is promoted from `warn` to `deny`.
#![deny(missing_docs)]

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
/// Compat workspace-adapter list entry.
pub mod workspace;

pub use event::{Envelope, Event};
pub use ids::{
    ActorClaim, ActorEpoch, ConfigGeneration, EventSeq, GoalId, LoopRunId, MemberId, MessageId,
    OperationId, OwnerRunId, PartId, PermissionRequestId, QuestionRequestId, SessionId, TeamRunId,
    ToolCallId,
};
pub use mail::{MailEndpoint, MailKind};
pub use message::{
    CostBreakdown, FinishReason, MemberRunStatus, Message, Part, Role, RosterStatus, SubagentMode,
    TokenUsage, ToolPartState,
};
pub use model::{AgentName, ModelRef, ToolName, ToolSchema};
pub use projection::{
    ChannelProjection, MailMessage, MemberProjection, MessageProjection, PartProjection,
    Projection, ResidentWorkProjection, RosterEntry, SessionProjection, TeamProjection,
};
pub use projection_tree::{RunTreeNode, build_run_tree};
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
