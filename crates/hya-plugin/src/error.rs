//! Errors surfaced by the plugin host and per-plugin client.
//!
//! Modeled on `hya_mcp::client::McpError` (the in-repo stdio JSON-RPC
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
    Rpc {
        /// JSON-RPC error code from the plugin.
        code: i64,
        /// Human-readable error message from the plugin.
        message: String,
    },
    /// The plugin announced a protocol version the host does not speak.
    #[error("plugin protocol version {got} != host {expected}")]
    ProtocolMismatch {
        /// Version the host required.
        expected: u32,
        /// Version the plugin declared.
        got: u32,
    },
    /// The child identity must match the stable configured source identity.
    #[error("plugin handshake id {got} != configured id {expected}")]
    IdentityMismatch {
        /// Configured / expected plugin id.
        expected: String,
        /// Id announced in the initialize reply.
        got: String,
    },
    /// A respawned child changed its complete initialize declaration.
    #[error("plugin declaration drift for {plugin}")]
    DeclarationDrift {
        /// Plugin id whose declaration changed across respawn.
        plugin: String,
    },
    /// A request exceeded its per-call timeout.
    #[error("plugin call timed out: {method}")]
    Timeout {
        /// Method that timed out.
        method: String,
    },
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
