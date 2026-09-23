//! Harness coordination tools (0.41.0).
//!
//! An agent's coordination tools are allocated by the harness when it starts,
//! the same way for built-in agents and for agents imported from bundles. A
//! bundle `resource_view` narrows only *domain* tools (read, write, edit, bash,
//! grep, MCP, skills, …); it cannot take away the tools an agent needs to take
//! part in a team:
//!
//! | Tool | Who gets it |
//! | --- | --- |
//! | `report` | every agent; advertised to subagents only (depth ≥ 1) |
//! | `wait` | every agent (the channel-tools override when that family is loaded) |
//! | `task`, `archive` | agents with spawn rights; hidden at the depth cap |
//! | `send`, `list_channel` | every agent, when the channel family is loaded |
//! | `read channel://…` | every agent, when the channel family is loaded; a view without `read` gets a mail-only `read` |
//!
//! `deny` may remove any of them except `report`: a subagent without `report`
//! could never finish. Denying `read` removes file reading only — the mail
//! read path stays.

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use hya_tool::{Tool, ToolCtx, ToolError, ToolResultPolicy};
use serde_json::{Value, json};

/// Canonical names of the coordination tools the harness injects.
pub(crate) const COORDINATION_TOOLS: [&str; 6] =
    ["report", "wait", "task", "archive", "send", "list_channel"];

/// Coordination tools that exist only for agents that can spawn.
pub(crate) const SPAWN_TOOLS: [&str; 2] = ["task", "archive"];

/// Coordination tools a `resource_view.deny` must not remove.
pub(crate) const UNDENIABLE_TOOLS: [&str; 1] = ["report"];

/// Tool whose presence in an agent's candidate pool means the channel family
/// (and therefore team mail) is loaded.
pub(crate) const CHANNEL_MARKER_TOOL: &str = "list_channel";

/// `read` restricted to `channel://` mail history, for a view that does not
/// select file reading.
pub(crate) struct ChannelReadTool {
    inner: Arc<dyn Tool>,
}

impl ChannelReadTool {
    pub(crate) fn new(inner: Arc<dyn Tool>) -> Self {
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
                "from `list_channel`, a `[NEW MAIL]` notice, or a rejected `report`. ",
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
        if !path.starts_with("channel://") {
            return Err(ToolError::Input(format!(
                "`{path}` is not a channel handle: this agent's `read` only serves team \
                 mail — use `channel://<id>` (see `list_channel` for ids)"
            )));
        }
        self.inner.execute(ctx, json!({ "path": path })).await
    }
}
