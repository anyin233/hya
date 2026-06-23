use serde_json::{Value, json};
use yaca_tool::ToolError;

pub(super) fn tool_error_value(error: &ToolError) -> Value {
    tool_error_message_value(tool_error_type(error), &error.to_string())
}

pub(super) fn tool_error_message_value(kind: &str, message: &str) -> Value {
    json!({
        "error": {
            "type": kind,
            "message": message,
        }
    })
}

fn tool_error_type(error: &ToolError) -> &'static str {
    match error {
        ToolError::Input(_) => "input",
        ToolError::Permission(_) => "permission",
        ToolError::Io(_) => "io",
        ToolError::Json(_) => "json",
        ToolError::Cancelled => "cancelled",
        ToolError::Other(_) => "unknown",
    }
}
