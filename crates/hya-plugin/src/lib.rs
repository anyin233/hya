//! `hya-plugin` — out-of-process plugin host for the hya harness.
//!
//! This crate owns the JSON-RPC 2.0 stdio protocol, the per-plugin client, the
//! `PluginHost` manager, and the bridges that implement the engine-facing
//! `hya_core::HookDispatcher` and `hya_tool::PermissionInterceptor` traits.
//!
//! Built out in phases (see
//! `.trellis/tasks/06-21-plugin-host-core/implement.md`). Phase 0 ships the
//! crate skeleton only.

pub mod client;
pub mod codec;
pub mod config;
pub mod dispatcher;
pub mod error;
pub mod host;
pub mod manifest;
pub mod messages;
pub mod permission_bridge;
mod plugin_tool;
pub mod protocol;

pub use client::{ChildGuard, DEFAULT_CALL_TIMEOUT, INITIALIZE_TIMEOUT, PluginClient};
pub use error::PluginError;
pub use host::{PluginHost, PluginStatus};
pub use messages::{HostInfo, PROTOCOL_VERSION};
pub use permission_bridge::PermissionBridge;
