//! `hya-plugin` — out-of-process plugin host for the hya harness.
//!
//! This crate owns the JSON-RPC 2.0 stdio protocol, the per-plugin client, the
//! [`PluginHost`] manager, and the bridges that implement the engine-facing
//! `hya_core::HookDispatcher` and `hya_tool::PermissionInterceptor` traits.
//!
//! External plugin authors implement the wire ABI documented in
//! `docs/plugin-protocol.md` (and mirrored by [`messages`] / [`protocol`]).
//! The host spawns each configured binary, runs `initialize`, then dispatches
//! tools and hooks over newline-delimited JSON on the child's stdio.

mod activation_dispatcher;
/// Per-process JSON-RPC client and child process lifecycle.
pub mod client;
/// Newline-delimited frame reader/writer over async stdio.
pub mod codec;
/// Config entries, manifests, and the merged [`config::PluginSpec`] list.
pub mod config;
/// Engine `HookDispatcher` that chains loaded plugins by declared load order.
pub mod dispatcher;
/// Errors from spawn, transport, handshake, and protocol mismatch.
pub mod error;
/// Multi-plugin manager: connect, restart budget, tools, and status.
pub mod host;
/// `plugin.toml` parse and hook resolution for directory-scanned plugins.
pub mod manifest;
/// Handshake, hook, and tool wire types plus method/protocol constants.
pub mod messages;
/// `permission.ask` bridge onto `hya_tool::PermissionInterceptor`.
pub mod permission_bridge;
mod plugin_tool;
/// JSON-RPC 2.0 frame types and standard error codes.
pub mod protocol;

pub use activation_dispatcher::ActivationHookDispatcher;
pub use client::{ChildGuard, DEFAULT_CALL_TIMEOUT, INITIALIZE_TIMEOUT, PluginClient};
pub use error::PluginError;
pub use host::{PluginHost, PluginStatus, PreparedPlugin};
pub use messages::{HostInfo, PROTOCOL_VERSION};
pub use permission_bridge::PermissionBridge;
