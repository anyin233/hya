//! Model Context Protocol (MCP) client integration for hya.
//!
//! Configured servers are prepared by spawning a stdio child process, running the
//! MCP initialize handshake, listing tools (and best-effort resources), and
//! registering each tool into the Harness tool plane under a namespaced name
//! `mcp__{server}__{tool}`. The model only sees those namespaced tools; permission
//! checks use `Action::Mcp` on the namespaced command string.
//!
//! Typical flow: parse [`McpServerConfig`] maps → [`prepare`] / [`McpManager::connect_all`]
//! → take [`McpManager::tools`] into the runtime registry. Deferred startup can hold
//! a [`McpManager::pending`] status map while connections finish in the background.

/// Tool bridge: MCP tool → [`hya_tool::Tool`] with namespacing and output shaping.
pub mod bridge;
/// JSON-RPC stdio client, child process guard, and client errors.
pub mod client;
/// Multi-server manager, config, prepare entry, and status snapshots.
pub mod manager;
/// Wire types for JSON-RPC and MCP tools/list / tools/call payloads.
pub mod protocol;
mod resource;

pub use client::{McpClient, McpError};
pub use manager::{McpManager, McpServerConfig, McpStatus, PreparedMcpServer, prepare};
