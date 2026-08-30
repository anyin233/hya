use hya_tool::ToolError;
use serde_json::{Value, json};

pub(super) fn tool_error_value(error: &ToolError) -> Value {
    match error {
        ToolError::WorkflowControl { code, message } => tool_error_message_value(code, message),
        _ => tool_error_message_value(tool_error_type(error), &error.to_string()),
    }
}

pub(super) fn tool_error_message_value(kind: &str, message: &str) -> Value {
    json!({
        "error": {
            "type": kind,
            "message": message,
        }
    })
}

fn tool_error_type(error: &ToolError) -> &str {
    match error {
        ToolError::Input(_) => "input",
        ToolError::Permission(_) => "permission",
        ToolError::Io(_) => "io",
        ToolError::Json(_) => "json",
        ToolError::Cancelled => "cancelled",
        ToolError::Overloaded(_) => "overloaded",
        ToolError::OperationIdConflict => "operation_id_conflict",
        ToolError::OperationAlreadyHandled => "operation_already_handled",
        ToolError::WorkflowControl { code, .. } => code,
        ToolError::UnknownAgentId { .. } => "unknown_agent_id",
        ToolError::AgentSpawnNotAllowed { .. } => "agent_spawn_not_allowed",
        ToolError::UnsupportedInlineAgentField { .. } => "unsupported_inline_agent_field",
        ToolError::Other(_) => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hya_tool::{Action, PermissionError, Resource};

    #[test]
    fn workflow_control_code_survives_tool_error_serialization() {
        let value = tool_error_value(&ToolError::WorkflowControl {
            code: "WORKFLOW_BUSY".to_string(),
            message: "another run is active".to_string(),
        });
        assert_eq!(value["error"]["type"], "WORKFLOW_BUSY");
        assert_eq!(value["error"]["message"], "another run is active");
    }

    #[test]
    fn every_tool_error_variant_has_a_stable_structured_type() {
        let json_error = match serde_json::from_str::<Value>("{") {
            Ok(_) => panic!("malformed JSON fixture unexpectedly parsed"),
            Err(error) => error,
        };
        let cases = vec![
            ("input", ToolError::Input("bad input".to_string())),
            (
                "permission",
                ToolError::Permission(PermissionError::Denied {
                    action: Action::Read,
                    resource: Resource::Path("blocked".to_string()),
                    feedback: None,
                }),
            ),
            (
                "permission",
                ToolError::Permission(PermissionError::Unavailable),
            ),
            ("io", ToolError::Io(std::io::Error::other("disk failure"))),
            ("json", ToolError::Json(json_error)),
            ("cancelled", ToolError::Cancelled),
            ("overloaded", ToolError::Overloaded("capacity".to_string())),
            ("operation_id_conflict", ToolError::OperationIdConflict),
            (
                "operation_already_handled",
                ToolError::OperationAlreadyHandled,
            ),
            (
                "WORKFLOW_BUSY",
                ToolError::WorkflowControl {
                    code: "WORKFLOW_BUSY".to_string(),
                    message: "another run is active".to_string(),
                },
            ),
            (
                "unknown_agent_id",
                ToolError::UnknownAgentId {
                    agent_id: "missing".to_string(),
                },
            ),
            (
                "agent_spawn_not_allowed",
                ToolError::AgentSpawnNotAllowed {
                    caller: "main".to_string(),
                    agent_id: "restricted".to_string(),
                },
            ),
            (
                "unsupported_inline_agent_field",
                ToolError::UnsupportedInlineAgentField {
                    field: "description",
                },
            ),
            ("unknown", ToolError::Other("other failure".to_string())),
        ];

        for (expected, error) in cases {
            let value = tool_error_value(&error);
            assert_eq!(value["error"]["type"], expected);
            assert!(
                value["error"]["message"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
            );
        }
    }
}
