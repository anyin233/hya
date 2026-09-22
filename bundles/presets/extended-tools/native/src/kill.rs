//! Parent-side subagent kill tool.
use hya_tool::tool::obj_schema;
use hya_tool::{Tool, ToolCtx, ToolError};
use serde_json::{Value, json};

/// Parent-side force kill (ADR-0015): archive a stuck direct child.
pub struct KillTool;

#[async_trait::async_trait]
impl Tool for KillTool {
    fn name(&self) -> &str {
        "kill"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        obj_schema(
            "kill",
            "Force-archive one of your direct subagents that is stuck or blocks your own report. The child is cancelled, a degraded handoff is written, and you receive its synthesized failure report.",
            json!({
                "handle": {
                    "type": "string",
                    "description": "The child handle to kill (from task or search_agent)"
                },
                "reason": {
                    "type": "string",
                    "description": "Short reason; delivered to you as the failure report"
                }
            }),
            &["handle"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let handle = input
            .get("handle")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| ToolError::Input("kill requires a handle".to_string()))?
            .to_string();
        let reason = input
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "killed by parent".to_string());
        ctx.lifecycle.kill(handle, reason).await?;
        Ok(json!({
            "title": "Killed",
            "output": "The child was archived with a synthesized failure report and a degraded handoff.",
        }))
    }
}
