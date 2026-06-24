use thiserror::Error;

/// Errors surfaced by the SDK layer.
///
/// Frozen contract (W0). `Spawn`/`Readiness` are populated in W1 once the real
/// `opencode serve` lifecycle lands; they exist now so the spawn-failure path
/// (PLAN.md S-8) has a typed home and downstream `match` arms are stable.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SdkError {
    #[error("failed to spawn `opencode serve`: {0}")]
    Spawn(String),

    #[error("server did not become ready within {0:?}")]
    Readiness(std::time::Duration),

    #[error("could not parse server listen URL from server output")]
    ListenUrlParse,

    #[error("http error: {0}")]
    Http(String),

    #[error("event stream error: {0}")]
    EventStream(String),

    #[error("native bridge error: {0}")]
    Bridge(String),

    #[error("native bridge protocol error: {0}")]
    Protocol(String),

    #[error("backend is not connected yet")]
    NotReady,

    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Convenience alias used across the SDK surface.
pub type Result<T> = std::result::Result<T, SdkError>;
