//! Dependency-inverted MCP control handle for Compat MCP HTTP routes.
//!
//! The server does not own an MCP manager. `hya-app` (or tests) supply an
//! [`McpControl`] implementation that mutates desired state and composes status
//! from desired/observed state plus the effective registry.

use std::collections::BTreeMap;

use futures::future::BoxFuture;
use hya_mcp::{McpServerConfig, McpStatus};
use serde_json::Value;

/// Process-owned MCP reconciliation API used by `/mcp*` Compat routes.
///
/// Implementations must be cheap to clone behind `Arc` and safe for concurrent
/// HTTP handlers. Errors are plain strings surfaced as HTTP 503 / error bodies
/// by the route layer.
pub trait McpControl: Send + Sync {
    /// Snapshot of each configured server's status (desired + observed).
    fn status(&self) -> BoxFuture<'_, BTreeMap<String, McpStatus>>;

    /// Insert or replace the named server config and reconcile.
    fn upsert(&self, name: String, config: McpServerConfig) -> BoxFuture<'_, Result<(), String>>;

    /// Enable or disable a named server; returns whether the name was known.
    fn set_enabled(&self, name: String, enabled: bool) -> BoxFuture<'_, Result<bool, String>>;

    /// MCP resources keyed for Compat resource listing routes.
    fn resources(&self) -> BoxFuture<'_, BTreeMap<String, Value>>;
}

pub(crate) struct EmptyMcpControl;

impl McpControl for EmptyMcpControl {
    fn status(&self) -> BoxFuture<'_, BTreeMap<String, McpStatus>> {
        Box::pin(async { BTreeMap::new() })
    }

    fn upsert(&self, _name: String, _config: McpServerConfig) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err("MCP reconciliation is unavailable".to_string()) })
    }

    fn set_enabled(&self, _name: String, _enabled: bool) -> BoxFuture<'_, Result<bool, String>> {
        Box::pin(async { Ok(false) })
    }

    fn resources(&self) -> BoxFuture<'_, BTreeMap<String, Value>> {
        Box::pin(async { BTreeMap::new() })
    }
}
