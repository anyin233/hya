use thiserror::Error;

/// Failures from the process E2E harness (not product assertions).
#[derive(Debug, Error)]
pub enum E2eError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("backend: {0}")]
    Backend(String),
    #[error("http: {0}")]
    Http(String),
    #[error("timeout waiting for {0}")]
    Timeout(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("client: {0}")]
    Client(String),
    #[error("{0}")]
    Other(String),
}

impl From<hya_client::ClientError> for E2eError {
    fn from(value: hya_client::ClientError) -> Self {
        Self::Client(value.to_string())
    }
}

impl From<reqwest::Error> for E2eError {
    fn from(value: reqwest::Error) -> Self {
        Self::Http(value.to_string())
    }
}
