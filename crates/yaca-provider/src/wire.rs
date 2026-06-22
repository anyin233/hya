//! Shared helpers for encoding stored tool parts into provider wire formats.

use serde_json::Value;
use yaca_proto::ToolPartState;

pub(crate) fn tool_input(state: &ToolPartState) -> &Value {
    match state {
        ToolPartState::Pending { input }
        | ToolPartState::Running { input }
        | ToolPartState::Completed { input, .. }
        | ToolPartState::Error { input, .. } => input,
    }
}

/// The tool's result as a plain string plus whether it was an error. A pending or
/// running state should not reach the encoder, but is handled so request pairing
/// (every call needs a result) never breaks.
pub(crate) fn tool_result(state: &ToolPartState) -> (String, bool) {
    match state {
        ToolPartState::Completed { output, .. } => (value_to_text(output), false),
        ToolPartState::Error { message, value, .. } => {
            (error_value_to_text(message, value.as_ref()), true)
        }
        ToolPartState::Running { .. } | ToolPartState::Pending { .. } => {
            ("(no result)".to_string(), true)
        }
    }
}

fn value_to_text(value: &Value) -> String {
    match value.as_str() {
        Some(s) => s.to_string(),
        None => value.to_string(),
    }
}

fn error_value_to_text(message: &str, value: Option<&Value>) -> String {
    match value {
        Some(Value::Array(_) | Value::Object(_)) => value
            .and_then(|v| serde_json::to_string(v).ok())
            .unwrap_or_else(|| message.to_string()),
        Some(Value::String(s)) => s.clone(),
        Some(value) => value.to_string(),
        None => message.to_string(),
    }
}
