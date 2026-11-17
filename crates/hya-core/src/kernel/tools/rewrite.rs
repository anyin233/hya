use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::{Tool, ToolCtx, ToolError};
use serde_json::{Value, json};

/// Removes everything before its own call from the Context. The kernel loop
/// intercepts calls by name; this type only advertises the schema.
pub(super) struct Rewrite;

/// Check a `rewrite` call's input: `summary` must be a string.
pub(in crate::kernel) fn check(input: &Value) -> Result<(), ToolError> {
    match input.get("summary") {
        Some(Value::String(_)) => Ok(()),
        _ => Err(ToolError::Input(
            "`summary` is required and must be a string".to_string(),
        )),
    }
}

#[async_trait]
impl Tool for Rewrite {
    fn name(&self) -> &str {
        super::REWRITE
    }

    fn schema(&self) -> ToolSchema {
        super::schema(
            super::REWRITE,
            "Remove everything before this call from the conversation. The summary stays, as your own text.",
            json!({
                "type": "object",
                "properties": {"summary": {"type": "string", "description": "What a later correction would need: the task, what was checked and how, what failed and why, what is still open"}},
                "required": ["summary"],
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Err(ToolError::Other(
            "rewrite runs only inside the kernel loop".to_string(),
        ))
    }
}
