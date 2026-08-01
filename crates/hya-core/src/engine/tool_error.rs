use hya_tool::ToolError;
use serde_json::{Value, json};

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
        ToolError::Overloaded(_) => "overloaded",
        ToolError::OperationIdConflict => "operation_id_conflict",
        ToolError::OperationAlreadyHandled => "operation_already_handled",
        ToolError::UnknownAgentId { .. } => "unknown_agent_id",
        ToolError::AgentSpawnNotAllowed { .. } => "agent_spawn_not_allowed",
        ToolError::UnsupportedInlineAgentField { .. } => "unsupported_inline_agent_field",
        ToolError::Other(_) => "unknown",
    }
}
