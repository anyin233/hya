use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use yaca_proto::{ToolName, ToolSchema};

use crate::interaction::{QuestionAnswer, QuestionKind};
use crate::tool::{Tool, ToolCtx, ToolError};

pub(crate) struct PlanExitTool;

#[derive(Deserialize)]
struct PlanExitInput {}

#[async_trait]
impl Tool for PlanExitTool {
    fn name(&self) -> &str {
        "plan_exit"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("plan_exit"),
            description: "Exit plan mode after asking the user to approve switching to build mode."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let _input: PlanExitInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let _session = ctx
            .session
            .ok_or_else(|| ToolError::Other("plan_exit requires a session".to_string()))?;

        let answer = ctx
            .interaction
            .ask(
                "Plan at current plan is complete. Would you like to switch to the build agent and start implementing?"
                    .to_string(),
                QuestionKind::Select {
                    options: vec!["Yes".to_string(), "No".to_string()],
                    allow_custom: false,
                },
            )
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?;

        if matches!(answer, QuestionAnswer::Selected(0)) {
            return Ok(json!({
                "title": "Switching to build agent",
                "output": "User approved switching to build agent. Wait for further instructions.",
                "metadata": {},
            }));
        }

        Err(ToolError::Other("plan exit rejected by user".to_string()))
    }
}
