//! Shared helpers for encoding stored tool parts into provider wire formats.

use hya_proto::ToolPartState;
use serde_json::Value;

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
    match value {
        Value::Object(fields) => fields
            .get("output")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| value.to_string()),
        Value::String(text) => text.clone(),
        _ => value.to_string(),
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
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn completed_object_prefers_string_output() {
        let output = json!({
            "title": "src/main.rs",
            "output": "plain replay text",
            "metadata": {"diff": "presentation-only"},
        });
        let state = ToolPartState::Completed {
            input: json!({"path": "src/main.rs"}),
            output,
            time_ms: 1,
        };
        assert_eq!(
            tool_result(&state),
            ("plain replay text".to_string(), false)
        );
    }

    #[test]
    fn completed_object_without_output_falls_back_to_json() {
        let output = json!({"title": "src/main.rs", "metadata": {"ok": true}});
        let expected = serde_json::to_string(&output).unwrap();
        let state = ToolPartState::Completed {
            input: Value::Null,
            output,
            time_ms: 1,
        };
        assert_eq!(tool_result(&state), (expected, false));
    }

    #[test]
    fn errors_keep_structured_json_behavior() {
        let value = json!({"output": "do not prefer errors", "type": "input"});
        let expected = serde_json::to_string(&value).unwrap();
        let state = ToolPartState::Error {
            input: Value::Null,
            message: "input: bad value".to_string(),
            value: Some(value),
        };
        assert_eq!(tool_result(&state), (expected, true));
    }
}
