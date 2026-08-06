//! JSON-RPC 2.0 wire frames over newline-delimited stdio (mirrors
//! `hya_mcp::protocol`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC version string required on every frame (`"2.0"`).
pub const JSONRPC_VERSION: &str = "2.0";

/// Standard and app-defined JSON-RPC error codes used on the plugin wire.
pub mod codes {
    /// Method is not implemented by the plugin (`-32601`).
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Malformed params (`-32602`).
    pub const INVALID_PARAMS: i64 = -32602;
    /// Plugin-side failure (`-32603`).
    pub const INTERNAL_ERROR: i64 = -32603;
    /// App-defined: a guard hook vetoed the action.
    pub const VETO: i64 = 1;
}

/// Host→plugin (or plugin→host) JSON-RPC request expecting a response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// Must be [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// Correlation id for the matching response.
    pub id: u64,
    /// Method name (`initialize`, `tool/call`, `hook/…`, …).
    pub method: String,
    /// Method params object (default empty JSON value).
    #[serde(default)]
    pub params: Value,
}

impl JsonRpcRequest {
    /// Build a request with `jsonrpc` set to [`JSONRPC_VERSION`].
    #[must_use]
    pub fn new(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// Reply to a [`JsonRpcRequest`]: success (`result`) or failure (`error`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// Must be [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// Same id as the request being answered.
    pub id: u64,
    /// Success payload when the call succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error object when the call failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// Successful response carrying `result`.
    #[must_use]
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Error response with the given code and message (no `data`).
    #[must_use]
    pub fn err(id: u64, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// JSON-RPC error object inside a response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Numeric error code (see [`codes`]).
    pub code: i64,
    /// Human-readable error message.
    pub message: String,
    /// Optional structured error details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// One-way JSON-RPC notification (no `id`, no reply expected).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    /// Must be [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// Notification method (for example `event`).
    pub method: String,
    /// Notification params.
    #[serde(default)]
    pub params: Value,
}

impl JsonRpcNotification {
    /// Build a notification with `jsonrpc` set to [`JSONRPC_VERSION`].
    #[must_use]
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

/// Classified NDJSON frame after [`Frame::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Request with both `method` and `id`.
    Request(JsonRpcRequest),
    /// Response with `result` xor `error` and an `id`.
    Response(JsonRpcResponse),
    /// Notification with `method` and no `id`.
    Notification(JsonRpcNotification),
}

impl Frame {
    /// # Errors
    /// Returns the parse / classification error message on malformed input.
    pub fn parse(line: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        let obj = value.as_object().ok_or("frame is not a JSON object")?;
        if obj.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC_VERSION) {
            return Err("invalid jsonrpc version".to_string());
        }
        if obj.contains_key("result") && obj.contains_key("error") {
            return Err("frame contains both result and error".to_string());
        }
        let is_request = obj.contains_key("method") && obj.contains_key("id");
        let is_notification = obj.contains_key("method") && !obj.contains_key("id");
        let is_response = obj.contains_key("result") || obj.contains_key("error");
        if is_request {
            serde_json::from_value(value)
                .map(Frame::Request)
                .map_err(|e| e.to_string())
        } else if is_notification {
            serde_json::from_value(value)
                .map(Frame::Notification)
                .map_err(|e| e.to_string())
        } else if is_response {
            serde_json::from_value(value)
                .map(Frame::Response)
                .map_err(|e| e.to_string())
        } else {
            Err("frame is neither request, response, nor notification".to_string())
        }
    }
}
