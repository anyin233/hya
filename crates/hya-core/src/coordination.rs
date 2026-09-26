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
        // `#<id>` / `#<member handle>`: the same channel read (this `read`
        // serves no files, so a `#` path cannot mean one).
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;

    use hya_proto::{SessionId, ToolName};
    use hya_tool::{
        InteractionPlane, LspPlane, MailboxPlane, PermissionPlane, PermissionRules, SkillPlane,
        SpawnerPlane, TodoPlane, WebSearchPlane, handle::ArtifactPlane,
    };
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// Echoes the path it was handed.
    struct Echo;

    #[async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "read"
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new("read"),
                description: String::new(),
                input_schema: json!({}),
                output_schema: None,
            }
        }
        async fn execute(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
            Ok(input["path"].clone())
        }
    }

    fn ctx() -> ToolCtx {
        let session = SessionId::new();
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![]));
        let (spawner, _srx) = SpawnerPlane::new();
        let (interaction, _irx) = InteractionPlane::new();
        ToolCtx {
            workflows: hya_tool::WorkflowPlane::disconnected(),
            permission: permission.for_session(session),
            interaction: interaction.for_session(session),
            spawner,
            operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
            mailbox: MailboxPlane::disconnected(),
            lifecycle: hya_tool::LifecyclePlane::disconnected(),
            session: Some(session),
            parent_session: None,
            todo: TodoPlane::default(),
            skills: SkillPlane::default(),
            artifacts: ArtifactPlane::default(),
            websearch: WebSearchPlane::default(),
            lsp: LspPlane::default(),
            formatter: hya_tool::FormatterPlane::default(),
            agents: Default::default(),
            workdir: PathBuf::from("."),
            roots: vec![PathBuf::from(".")],
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn the_mail_only_read_serves_hash_paths_as_channel_reads() {
        let tool = ChannelReadTool::new(Arc::new(Echo));
        let ctx = ctx();
        for (path, forwarded) in [
            ("#main/scout-a", "channel://#main/scout-a"),
            ("#DM-aB12Cd34", "channel://#DM-aB12Cd34"),
            ("channel://main/scout-a", "channel://main/scout-a"),
        ] {
            let out = tool.execute(&ctx, json!({ "path": path })).await.unwrap();
            assert_eq!(out, forwarded, "{path}");
        }
        let error = tool
            .execute(&ctx, json!({ "path": "src/lib.rs" }))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ToolError::Input(message) if message.contains("member handle")),
            "{error:?}"
        );
    }
}
