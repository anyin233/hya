//! Runtime failures returned by the kernel and by the session engine built on it.

use std::sync::Arc;

use thiserror::Error;

/// Top-level error for session/turn/orchestration paths.
#[derive(Clone, Error, Debug)]
pub enum CoreError {
    /// Bundle catalog or package resolution failed: the engine's
    /// `hya_bundle::BundleError`.
    #[error(transparent)]
    Bundle(Arc<dyn std::error::Error + Send + Sync>),
    /// Model provider transport or protocol failure (shared via `Arc` for clone).
    #[error(transparent)]
    Provider(Arc<hya_provider::ProviderError>),
    /// Tool execution or permission failure (shared via `Arc` for clone).
    #[error(transparent)]
    Tool(Arc<hya_tool::ToolError>),
    /// SQLite store / projection failure.
    #[error(transparent)]
    Store(#[from] hya_store::StoreError),
    /// Runtime candidate publication or refresh failed: the engine's
    /// `RuntimeRefreshError`.
    #[error(transparent)]
    RuntimeRefresh(Arc<dyn std::error::Error + Send + Sync>),
    /// Cooperative cancellation (turn token or takeover).
    #[error("cancelled")]
    Cancelled,
    /// Requested agent id is not present in the bound catalog.
    #[error("AGENT_DEFINITION_MISSING: `{agent_id}`")]
    AgentDefinitionMissing {
        /// Missing agent stable id.
        agent_id: String,
    },
    /// A Workflow actor reported failure or ended before successful settlement.
    #[error("{0}")]
    WorkflowActivationFailed(String),
    /// Caller-supplied parameters or preflight checks failed.
    #[error("invalid: {0}")]
    Invalid(String),
    /// One bundle's runtime generation could not be prepared (its process,
    /// MCP server, declarations, or configuration); the refresh that hit it
    /// keeps the previous generation published.
    #[error("bundle `{bundle_id}`: {source}")]
    BundleRuntime {
        /// Stable id of the bundle that failed.
        bundle_id: String,
        /// Why it failed.
        source: Box<CoreError>,
    },
    /// Independent refresh failures, kept as source id / diagnostic pairs
    /// across the single-error catalog-refresh trait boundary.
    #[error("bundle refresh failed: {}", .0.iter().map(|(id, error)| format!("{id}: {error}")).collect::<Vec<_>>().join("; "))]
    BundleRefresh(Vec<(String, String)>),
    /// The session already has an active turn; a session runs at most one
    /// turn at a time (single-active-turn invariant).
    #[error("TURN_ALREADY_ACTIVE: session `{session}` already has an active turn")]
    TurnAlreadyActive {
        /// The busy session.
        session: hya_proto::SessionId,
    },
    /// Spawning or reviving a member would exceed the root session's live capacity.
    #[error("CAPACITY_EXCEEDED: requested {requested}, limit {limit}")]
    CapacityExceeded {
        /// Requested member slots.
        requested: u64,
        /// Configured live member limit.
        limit: u64,
    },
    /// Mail authorization rejected at durable commit time.
    #[error("MAIL_AUTHORIZATION_REJECTED: {sender} -> {channel}: {reason}")]
    MailAuthorizationRejected {
        /// Sender handle.
        sender: String,
        /// Target channel or address.
        channel: String,
        /// Rejection detail.
        reason: String,
    },
    /// Mail target is archived and requires revival before delivery.
    #[error("MAIL_REVIVAL_REQUIRED: {sender} -> {channel} ({target})")]
    MailRevivalRequired {
        /// Sender handle.
        sender: String,
        /// Target channel id.
        channel: String,
        /// Archived recipient handle.
        target: String,
    },
    /// Mailbox or report gate rejected the operation.
    #[error("{0}")]
    MailboxRejected(String),
}

impl CoreError {
    /// Wrap a bundle catalog or package error as [`CoreError::Bundle`].
    pub fn bundle(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Bundle(Arc::new(error))
    }

    /// Wrap a runtime refresh error as [`CoreError::RuntimeRefresh`].
    pub fn runtime_refresh(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::RuntimeRefresh(Arc::new(error))
    }
}

impl From<hya_provider::ProviderError> for CoreError {
    fn from(error: hya_provider::ProviderError) -> Self {
        Self::Provider(Arc::new(error))
    }
}

impl From<hya_tool::ToolError> for CoreError {
    fn from(error: hya_tool::ToolError) -> Self {
        Self::Tool(Arc::new(error))
    }
}
