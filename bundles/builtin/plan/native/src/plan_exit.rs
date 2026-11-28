//! Interactive `plan_exit` tool requesting user approval to leave plan mode.

use async_trait::async_trait;
use hya_proto::ToolSchema;
use hya_tool::tool::obj_schema;
use hya_tool::{QuestionAnswer, QuestionKind, Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

/// Canonical tool name for exiting plan mode.
pub const PLAN_EXIT_TOOL_NAME: &str = "plan_exit";

/// Prompts the user to approve switching from `hya-plan` to the build agent.
pub struct PlanExitTool;

#[derive(Deserialize)]
struct PlanExitInput {}

#[async_trait]
impl Tool for PlanExitTool {
    fn name(&self) -> &str {
        PLAN_EXIT_TOOL_NAME
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            PLAN_EXIT_TOOL_NAME,
            "Exit plan mode after asking the user to approve switching to build mode.",
            json!({}),
            &[],
        )
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
