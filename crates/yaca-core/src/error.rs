use thiserror::Error;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error(transparent)]
    Provider(#[from] yaca_provider::ProviderError),
    #[error(transparent)]
    Tool(#[from] yaca_tool::ToolError),
    #[error(transparent)]
    Store(#[from] yaca_store::StoreError),
    #[error("cancelled")]
    Cancelled,
}
