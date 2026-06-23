//! Errors surfaced by the plugin host and per-plugin client.
//!
//! Modeled on `yaca_mcp::client::McpError` (the in-repo stdio JSON-RPC
//! precedent) so the two crates can later share a transport helper.

use thiserror::Error;

/// An error from spawning, talking to, or decoding a plugin child process.
#[derive(Error, Debug, Clone)]
pub enum PluginError {
    /// The configured plugin `command` had no program to spawn.
    #[error("plugin command is empty")]
    EmptyCommand,
    /// A required child stdio pipe (`stdin`/`stdout`) was unavailable.
    #[error("plugin stdio unavailable: {0}")]
    MissingPipe(&'static str),
    /// An underlying I/O failure (kept as a string so the error stays `Clone`).
    #[error("io: {0}")]
    Io(String),
    /// A (de)serialization failure on a wire frame.
    #[error("json: {0}")]
    Json(String),
    /// The plugin replied with a JSON-RPC error object.
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    /// The plugin announced a protocol version the host does not speak.
    #[error("plugin protocol version {got} != host {expected}")]
    ProtocolMismatch { expected: u32, got: u32 },
    /// A request exceeded its per-call timeout.
    #[error("plugin call timed out: {method}")]
    Timeout { method: String },
    /// The plugin connection closed (EOF / crash) with requests in flight.
    #[error("plugin connection closed")]
    Closed,
    /// The plugin exceeded its restart budget and is no longer being respawned.
    #[error("plugin disabled after exceeding restart budget")]
    Disabled,
    /// A single JSONL frame exceeded the maximum line length.
    #[error("plugin line exceeded {0} bytes")]
    OversizedLine(usize),
}
