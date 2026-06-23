//! `yaca-proto` — wire types shared by client + server.
//!
//! Two invariants (design.md §3): tagged enums everywhere (never `untagged`) and
//! a newtype per id. This crate is dependency-light (serde / uuid only) so the
//! TUI can share types without pulling sqlx/tokio into its build graph.

pub mod api;
pub mod event;
pub mod ids;
pub mod message;
pub mod model;
pub mod projection;
pub mod workspace;

pub use event::{Envelope, Event};
pub use ids::{
    EventSeq, GoalId, LoopRunId, MemberId, MessageId, PartId, PermissionRequestId,
    QuestionRequestId, SessionId, TeamRunId, ToolCallId,
};
pub use message::{CostBreakdown, FinishReason, Message, Part, Role, TokenUsage, ToolPartState};
pub use model::{AgentName, ModelRef, ToolName, ToolSchema};
pub use projection::{MessageProjection, PartProjection, Projection, SessionProjection};
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
