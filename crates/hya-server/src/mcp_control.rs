use std::collections::BTreeMap;

use futures::future::BoxFuture;
use hya_mcp::{McpServerConfig, McpStatus};
use serde_json::Value;

pub trait McpControl: Send + Sync {
    fn status(&self) -> BoxFuture<'_, BTreeMap<String, McpStatus>>;

    fn upsert(&self, name: String, config: McpServerConfig) -> BoxFuture<'_, Result<(), String>>;

    fn set_enabled(&self, name: String, enabled: bool) -> BoxFuture<'_, Result<bool, String>>;

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
