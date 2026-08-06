//! Harness failures (spawn, HTTP, timeouts) — distinct from product asserts.

use thiserror::Error;

/// Failures from the process E2E harness (not product assertions).
#[derive(Debug, Error)]
pub enum E2eError {
    /// Filesystem or process I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Backend binary missing, spawn failure, or bad exit.
    #[error("backend: {0}")]
    Backend(String),
    /// REST call non-success or transport failure.
    #[error("http: {0}")]
    Http(String),
    /// Polling wait exceeded budget (condition name in message).
    #[error("timeout waiting for {0}")]
    Timeout(String),
    /// JSON encode/decode failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Typed `hya-client` failure.
    #[error("client: {0}")]
    Client(String),
    /// Catch-all harness message.
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
