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
/// Streamable HTTP and classic HTTP+SSE transports.
mod http;
/// Server configuration, preparation and connection status.
pub mod manager;
/// Wire types for JSON-RPC and MCP tools/list / tools/call payloads.
pub mod protocol;
mod resource;

pub use client::{McpClient, McpError};
pub use manager::{McpServerConfig, McpStatus, PreparedMcpServer, prepare, prepare_bundle};
