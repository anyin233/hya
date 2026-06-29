use thiserror::Error;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error(transparent)]
    Provider(#[from] hya_provider::ProviderError),
    #[error(transparent)]
    Tool(#[from] hya_tool::ToolError),
    #[error(transparent)]
    Store(#[from] hya_store::StoreError),
    #[error("cancelled")]
    Cancelled,
    #[error("invalid: {0}")]
    Invalid(String),
}
