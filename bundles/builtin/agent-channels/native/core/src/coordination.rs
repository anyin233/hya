//! Harness coordination tool constants and mail-only `read` wrapper (0.41.0, ADR-0039).

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use hya_tool::{Tool, ToolCtx, ToolError, ToolResultPolicy};
use serde_json::{Value, json};

/// Canonical names of the coordination tools the harness injects.
pub const COORDINATION_TOOLS: [&str; 6] =
    ["report", "wait", "task", "archive", "send", "list_channel"];

/// Coordination tools that exist only for agents that can spawn.
pub const SPAWN_TOOLS: [&str; 2] = ["task", "archive"];

/// Coordination tools a `resource_view.deny` must not remove.
pub const UNDENIABLE_TOOLS: [&str; 1] = ["report"];

/// Tool whose presence in an agent's candidate pool means `hya/agent-channels`
/// (and therefore team mail) is loaded.
pub const CHANNEL_MARKER_TOOL: &str = "list_channel";

/// `read` restricted to `channel://` mail history, for a view that does not
/// select file reading.
pub struct ChannelReadTool {
    inner: Arc<dyn Tool>,
}

impl ChannelReadTool {
    /// Wrap an underlying `read` tool so only `channel://` (and `#<id>`) paths
    /// are forwarded.
    #[must_use]
    pub fn new(inner: Arc<dyn Tool>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl Tool for ChannelReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("read"),
            description: concat!(
                "Read team mail history. `path` is `channel://<id>` for the latest ",
                "message or `channel://<id>?last=N` for the last N; channel ids come ",
                "from `list_channel`, a `[NEW MAIL]` notice, or a rejected `report`; ",
                "a member handle (`channel://main/scout-suzuran`) reads your DM with it. ",
                "Reading marks your inbox seen. This agent has no file reading: any ",
                "other path is refused."
            )
            .to_string(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "`channel://<id>` or `channel://<id>?last=N`"
                    }
                },
                "required": ["path"]
            }),
            output_schema: None,
        }
    }

    fn result_policy(&self) -> ToolResultPolicy {
        self.inner.result_policy()
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let path = input
            .get("path")
            .or_else(|| input.get("filePath"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let path = match path.strip_prefix('#') {
            Some(_) => format!("channel://{path}"),
            None if path.starts_with("channel://") => path.to_string(),
            None => {
                return Err(ToolError::Input(format!(
                    "`{path}` is not a channel handle: this agent's `read` only serves team \
                     mail — use `channel://<id>` (see `list_channel` for ids) or \
                     `channel://<member handle>` for your DM with that member"
                )));
            }
        };
        self.inner.execute(ctx, json!({ "path": path })).await
    }
}
