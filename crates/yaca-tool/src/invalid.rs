use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use yaca_proto::{ToolName, ToolSchema};

use crate::tool::{Tool, ToolCtx, ToolError};

pub(crate) struct InvalidTool;

#[derive(Deserialize)]
struct InvalidInput {
    #[serde(rename = "tool")]
    _tool: String,
    error: String,
}

#[async_trait]
impl Tool for InvalidTool {
    fn name(&self) -> &str {
        "invalid"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("invalid"),
            description: "Do not use.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "tool": { "type": "string" },
                    "error": { "type": "string" }
                },
                "required": ["tool", "error"]
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: InvalidInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        Ok(json!({
            "title": "Invalid Tool",
            "output": format!("The arguments provided to the tool are invalid: {}", input.error),
            "metadata": {},
        }))
    }
}
