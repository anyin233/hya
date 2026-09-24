//! The `wait` tool: block until subagents finish — report or are archived; idle never counts — or new mail for the caller arrives (harness mail included; each message is returned once and marked read); overrides the extended-tools `wait` via `overrides: hya/extended-tools` in this family's exposure policy.
use hya_tool::{Tool, ToolCtx, ToolError, WaitSpec, wait_tool_schema};
use serde_json::Value;

/// Wake on mail too.
const WAKE_ON_MAIL: bool = true;

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
