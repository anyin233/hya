//! The `wait` tool: block until subagents finish (member progress only; the channel-tools family overrides it with a version that also wakes on mail).
use hya_tool::{Tool, ToolCtx, ToolError, WaitSpec, wait_tool_schema};
use serde_json::Value;

/// Wake on mail too.
const WAKE_ON_MAIL: bool = false;

/// Block until the caller's subagents finish their current work.
pub struct WaitTool;

#[async_trait::async_trait]
impl Tool for WaitTool {
    fn name(&self) -> &str {
        "wait"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        wait_tool_schema(WAKE_ON_MAIL)
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let spec = WaitSpec::parse(&input, WAKE_ON_MAIL)?;
        let outcome = ctx.lifecycle.wait(spec, &ctx.cancel).await?;
        Ok(outcome.to_tool_result())
    }
}
