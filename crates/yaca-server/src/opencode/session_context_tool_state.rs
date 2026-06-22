use serde_json::{Value, json};
use yaca_proto::ToolPartState;

pub(in crate::opencode) fn tool_state(state: &ToolPartState) -> Value {
    match state {
        ToolPartState::Pending { input } => json!({
            "status": "pending",
            "input": input.to_string(),
        }),
        ToolPartState::Running { input } => json!({
            "status": "running",
            "input": input,
            "structured": {},
            "content": [],
        }),
        ToolPartState::Completed { input, output, .. } => json!({
            "status": "completed",
            "input": input,
            "content": [],
            "outputPaths": [],
            "structured": {},
            "result": output,
        }),
        ToolPartState::Error {
            input,
            message,
            value,
        } => json!({
            "status": "error",
            "input": input,
            "content": [],
            "structured": {},
            "error": { "name": "ToolError", "message": message },
            "result": value.clone().unwrap_or_else(|| json!(message)),
        }),
    }
}

pub(in crate::opencode) fn tool_provider(state: &ToolPartState) -> Option<Value> {
    match state {
        ToolPartState::Pending { .. } => None,
        ToolPartState::Running { .. }
        | ToolPartState::Completed { .. }
        | ToolPartState::Error { .. } => Some(json!({ "executed": true })),
    }
}
