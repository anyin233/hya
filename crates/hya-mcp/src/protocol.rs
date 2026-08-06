//! JSON-RPC 2.0 and MCP tools wire shapes used on the stdio transport.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Outbound JSON-RPC request written as one newline-delimited line on MCP stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// Protocol version string; always `"2.0"` for this client.
    pub jsonrpc: String,
    /// Correlation id matched against the response `id`.
    pub id: u64,
    /// MCP method name (for example `tools/list`, `tools/call`, `initialize`).
    pub method: String,
    /// Method parameters object (or other JSON value).
    #[serde(default)]
    pub params: Value,
}

/// Inbound JSON-RPC response demuxed by `id` from MCP stdout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// Protocol version string from the server.
    pub jsonrpc: String,
    /// Must match the request id waiting in the client pending map.
    pub id: u64,
    /// Successful result payload when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// JSON-RPC error object when the call failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC error object from a failed MCP call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Numeric error code from the server.
    pub code: i64,
    /// Human-readable error message.
    pub message: String,
    /// Optional structured error data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// One tool advertised by `tools/list` (MCP camelCase on the wire).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    /// Server-local tool name (not yet namespaced for the model).
    pub name: String,
    /// Model-facing description; may be empty.
    #[serde(default)]
    pub description: String,
    /// JSON Schema for arguments; must be `type: "object"` to register as a tool.
    pub input_schema: Value,
}

/// Result body of `tools/list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsListResult {
    /// Tools published by the server.
    pub tools: Vec<ToolInfo>,
}

/// Result body of `tools/call` (MCP camelCase `isError`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    /// Content array or other MCP content value returned by the tool.
    pub content: Value,
    /// When true, the bridge surfaces a tool error instead of success output.
    #[serde(default)]
    pub is_error: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tools_list_round_trips_mcp_shape() {
        let result = ToolsListResult {
            tools: vec![ToolInfo {
                name: "ping".to_string(),
                description: "Ping".to_string(),
                input_schema: json!({ "type": "object", "properties": {} }),
            }],
        };

        let encoded = serde_json::to_string(&result).expect("serialize tools list");
        assert!(encoded.contains("inputSchema"));
        let decoded: ToolsListResult = serde_json::from_str(&encoded).expect("decode tools list");
        assert_eq!(decoded, result);
    }
}
