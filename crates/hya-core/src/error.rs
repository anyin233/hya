//! Runtime failures returned by [`crate::SessionEngine`] and orchestration helpers.

use std::sync::Arc;

use thiserror::Error;

/// Top-level error for session/turn/orchestration paths.
#[derive(Clone, Error, Debug)]
pub enum CoreError {
    /// Bundle catalog or package resolution failed.
    #[error(transparent)]
    Bundle(#[from] hya_bundle::BundleError),
    /// Model provider transport or protocol failure (shared via `Arc` for clone).
    #[error(transparent)]
    Provider(Arc<hya_provider::ProviderError>),
    /// Tool execution or permission failure (shared via `Arc` for clone).
    #[error(transparent)]
    Tool(Arc<hya_tool::ToolError>),
    /// SQLite store / projection failure.
    #[error(transparent)]
    Store(#[from] hya_store::StoreError),
    /// Runtime candidate publication or refresh failed.
    #[error(transparent)]
    RuntimeRefresh(#[from] crate::RuntimeRefreshError),
    /// Cooperative cancellation (turn token or takeover).
    #[error("cancelled")]
    Cancelled,
    /// Requested agent id is not present in the bound catalog.
    #[error("AGENT_DEFINITION_MISSING: `{agent_id}`")]
    AgentDefinitionMissing {
        /// Missing agent stable id.
        agent_id: String,
    },
    /// Caller-supplied parameters or preflight checks failed.
    #[error("invalid: {0}")]
    Invalid(String),
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
