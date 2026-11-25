//! Model Context Protocol (MCP) client integration for hya.
//!
//! Configured servers are prepared by spawning a stdio child process, running the
//! MCP initialize handshake, listing tools (and best-effort resources), and
//! registering each tool into the Harness tool plane under a namespaced name
//! `mcp__{server}__{tool}`. The model only sees those namespaced tools; permission
//! checks use `Action::Mcp` on the namespaced command string.
//!
//! Prepare each named server using [`prepare`]. Keep the returned
//! [`PreparedMcpServer`] alive while its tools and resources are published.
//! Packaged bundles use [`prepare_bundle`] to isolate environment and cwd while
//! keeping ordinary configured MCP startup behavior unchanged.

/// Tool bridge: MCP tool → [`hya_tool::Tool`] with namespacing and output shaping.
pub mod bridge;
/// JSON-RPC client over stdio/HTTP transports, child process guard, and errors.
pub mod client;
/// Bounded NDJSON framing shared with native plugins.
#[deprecated(note = "use hya_child_io::codec")]
pub mod codec {
    pub use hya_child_io::codec::*;
}
/// Streamable HTTP and classic HTTP+SSE transports.
mod http;
/// Composition completion tracking for connection I/O.
#[deprecated(note = "use hya_child_io::io_tasks")]
pub mod io_tasks {
    pub use hya_child_io::io_tasks::*;
}
/// Server configuration, preparation and connection status.
pub mod manager;
/// Wire types for JSON-RPC and MCP tools/list / tools/call payloads.
pub mod protocol;
mod resource;

pub use client::{McpClient, McpError};
pub use manager::{McpServerConfig, McpStatus, PreparedMcpServer, prepare, prepare_bundle};
