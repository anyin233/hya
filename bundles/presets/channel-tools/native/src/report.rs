//! Terminal subagent report tool.
use hya_proto::ReportOutcome;
use hya_tool::tool::obj_schema;
use hya_tool::{Tool, ToolCtx, ToolError};
use serde_json::{Value, json};

/// Terminal report tool (ADR-0015): ends the calling agent's episode.
pub struct ReportTool;

#[async_trait::async_trait]
impl Tool for ReportTool {
    fn name(&self) -> &str {
        "report"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        obj_schema(
            "report",
            "Deliver your terminal report and end your episode. The engine checks you have no unread mail and no live children (answer or archive them first), writes your state handoff, delivers this report to your parent, and archives you. A follow-up from your parent can revive you with that handoff context.",
            json!({
                "result": {
                    "type": "string",
                    "description": "The result summary your parent receives"
                },
                "outcome": {
                    "type": "string",
                    "enum": ["done", "failed"],
                    "description": "Whether the task succeeded (default done)"
                }
            }),
            &["result"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let result = input
            .get("result")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| ToolError::Input("report requires a non-empty result".to_string()))?;
        let outcome = match input.get("outcome").and_then(Value::as_str) {
            Some("failed") => ReportOutcome::Failed,
            _ => ReportOutcome::Done,
        };
        ctx.lifecycle.report(outcome, result).await?;
        Ok(json!({
            "title": "Report accepted",
            "output": "Report accepted. Your episode ends when this turn completes; you will be archived with a state handoff. Your parent can revive you later.",
        }))
    }
}
