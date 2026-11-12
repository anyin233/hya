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
            "Submit your terminal report; it takes effect when this turn ends. The engine checks you have no unread mail and no live children (answer or archive them first). Submission ends your turn right after this tool round (no further model call). The engine then rechecks the gate, writes your state handoff, delivers the report to your parent, and archives you. If new mail, a live subagent, or a changed episode prevents archive, you receive a harness notice and must report again after handling it. Call it once per turn. Mail from your parent can wake you after archive for a new episode with that handoff context.",
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
            "title": "Report submitted",
            "output": "Report submitted; it takes effect when this turn ends. If new mail or a live subagent prevents it, you will receive a harness notice and must report again. After archive, mail from your parent can wake you for a new episode with your state handoff.",
        }))
    }
}
