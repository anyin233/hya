use thiserror::Error;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error(transparent)]
    Bundle(#[from] hya_bundle::BundleError),
    #[error(transparent)]
    Provider(#[from] hya_provider::ProviderError),
    #[error(transparent)]
    Tool(#[from] hya_tool::ToolError),
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
