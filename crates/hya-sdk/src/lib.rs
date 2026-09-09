//! Typed client, wire types, live stores, and reducers for the hya backend.
//!
//! This crate talks to **`hya-server`** (and its Compat-compatible routes) over
//! HTTP/SSE, or through any other [`Transport`] that presents the same
//! [`Client`] surface (see the `hya-native` crate for the in-process router
//! transport). It is the TUI/integration SDK: session CRUD, prompts,
//! permissions, global events, projected message stores, and the turn-stream
//! timeline reducer — not the native `/sessions` micro-client in `hya-client`.

/// HTTP header that scopes each request to a working directory. This is the single wire-protocol
/// string coupled to the current backend; change it here (or translate it in the transport)
/// when porting to a different backend.
pub const DIRECTORY_HEADER: &str = "x-opencode-directory";

/// SDK error types and [`Result`] alias.
pub mod error;
/// Global SSE event streaming helpers.
pub mod events;
/// Turn-stream (`session.next.*`) timeline projection.
pub mod reducer;
/// Owned `hya-backend serve` subprocess lifecycle.
pub mod server;
/// Live message/part/session store folded from global events.
pub mod store;
/// Team mail/channel/roster projections.
pub mod team;
/// Serde wire types for config, sessions, messages, and events.
pub mod types;
/// Typed Workflow commands, results, and replay state.
pub mod workflow;
pub use workflow::{
    MemberId, OwnerRunId, WorkflowAvailability, WorkflowCommand, WorkflowCommandResult,
    WorkflowDelivery, WorkflowIdentity, WorkflowInfo, WorkflowMemberProjection, WorkflowMemberRole,
    WorkflowModelAssignment, WorkflowModelCandidate, WorkflowModelResolvedCandidate,
    WorkflowProjection, WorkflowRevision, WorkflowRouteFailureClass, WorkflowRunId,
    WorkflowRunProjection, WorkflowRunResult, WorkflowRunStatus, WorkflowSourceId,
    WorkflowStageInfo, WorkflowStagePlan, WorkflowStageProjection, WorkflowStageRouteOutcome,
    WorkflowStageStatus, WorkflowSummary,
};

mod client;
pub use client::{ApiClient, Client, HttpClient, Transport};
pub use error::{decode_http_error, SdkError, SdkHttpError};
pub use events::stream_global_events;
pub use reducer::{Data, V2Event};
pub use server::{default_session_db_path, hya_state_dir, ServerHandle, HYA_DB_ENV};
pub use store::{MemberProjection, MessageStore, StoredPart};
pub use team::{ChannelProjection, MailEndpoint, MailMessage, RosterEntry, TeamProjection};
pub use types::{
    Agent, Config, EventPayload, GlobalEvent, Message, MessageTime, Part, Session, SessionMessage,
    ToolPart,
};
