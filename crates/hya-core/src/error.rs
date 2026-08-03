use std::sync::Arc;

use thiserror::Error;

#[derive(Clone, Error, Debug)]
pub enum CoreError {
    #[error(transparent)]
    Bundle(#[from] hya_bundle::BundleError),
    #[error(transparent)]
    Provider(Arc<hya_provider::ProviderError>),
    #[error(transparent)]
    Tool(Arc<hya_tool::ToolError>),
    #[error(transparent)]
    Store(#[from] hya_store::StoreError),
    #[error(transparent)]
    RuntimeRefresh(#[from] crate::RuntimeRefreshError),
    #[error("cancelled")]
    Cancelled,
    #[error("AGENT_DEFINITION_MISSING: `{agent_id}`")]
    AgentDefinitionMissing { agent_id: String },
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
