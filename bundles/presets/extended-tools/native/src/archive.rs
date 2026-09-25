//! Stop-and-archive a subagent (replaces the removed `kill` tool).
use hya_tool::tool::obj_schema;
use hya_tool::{Tool, ToolCtx, ToolError};
use serde_json::{Value, json};

/// The `archive` tool: stop one of the caller's subagents and archive it.
pub struct ArchiveTool;

#[async_trait::async_trait]
impl Tool for ArchiveTool {
    fn name(&self) -> &str {
        "archive"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        obj_schema(
            "archive",
            "Stop one of your subagents and archive it: its in-flight turn is cancelled, a state handoff is written, and it leaves your live roster (its own subagents are archived first). Use it for a subagent that is stuck, no longer needed, or blocking your own report. An archived subagent stays readable (session log, channel history) and is NOT gone: sending mail to its handle wakes it again with the same handle and session. The team lead (`main`) can never be archived.",
            json!({
                "target": {
                    "type": "string",
                    "description": "The subagent to archive: its handle as returned by `task` (e.g. `main/hya-worker-exusiai`, or just the leaf `hya-worker-exusiai` for your own subagent) or its session id (`hysec_...`)."
                },
                "reason": {
                    "type": "string",
                    "description": "Short reason, recorded on its member row (optional)."
                }
            }),
            &["target"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let target = input
            .get("target")
            .or_else(|| input.get("handle"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                ToolError::Input(
                    "archive requires `target`: the subagent's handle (e.g. `main/hya-worker-exusiai`) or session id from the `task` result".to_string(),
                )
            })?
            .to_string();
        let reason = input
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default();
        let receipt = ctx.lifecycle.archive(target, reason).await?;
        let mut output = format!(
            "Archived `{}` (session {}).",
            receipt.handle, receipt.session
        );
        if receipt.cancelled_turn {
            output.push_str(" Its in-flight turn was cancelled.");
        }
        if !receipt.descendants.is_empty() {
            output.push_str(&format!(
                " Its subagents were archived first: {}.",
                receipt.descendants.join(", ")
            ));
        }
        output.push_str(&format!(
            " Send mail to `{}` to wake it again with its handoff.",
            receipt.handle
        ));
        Ok(json!({
            "title": format!("Archived {}", receipt.handle),
            "output": output,
            "metadata": receipt,
        }))
    }
}
