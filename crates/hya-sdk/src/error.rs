//! Errors returned by transport, spawn, and decode paths.

use thiserror::Error;

/// Errors surfaced by the SDK layer.
///
/// Covers owned-backend spawn readiness, HTTP/SSE transport, the native bridge,
/// and JSON decode. Variants are `#[non_exhaustive]` so new failure modes can
/// land without breaking `match` arms that use a wildcard.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SdkError {
    /// Failed to start the backend subprocess.
    #[error("failed to spawn `compat serve`: {0}")]
    Spawn(String),

    /// Subprocess started but never printed a parseable listen URL in time.
    #[error("server did not become ready within {0:?}")]
    Readiness(std::time::Duration),

    /// Server exited or logged without a `listening on http://…` line.
    #[error("could not parse server listen URL from server output")]
    ListenUrlParse,

    /// Non-success HTTP status or transport failure on a REST call.
    #[error("http error: {0}")]
    Http(String),

    /// SSE / global event stream failure.
    #[error("event stream error: {0}")]
    EventStream(String),

    /// Native bridge process or IPC failure.
    #[error("native bridge error: {0}")]
    Bridge(String),

    /// Native bridge sent an unexpected protocol message.
    #[error("native bridge protocol error: {0}")]
    Protocol(String),

    /// Call made before the backend connection was established.
    #[error("backend is not connected yet")]
    NotReady,

    /// JSON encode/decode failure.
    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Convenience alias used across the SDK surface.
pub type Result<T> = std::result::Result<T, SdkError>;
