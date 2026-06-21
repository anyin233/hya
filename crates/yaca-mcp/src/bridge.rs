use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use yaca_proto::{ToolName, ToolSchema};
use yaca_tool::{Action, Resource, Tool, ToolCtx, ToolError};

use crate::client::McpClient;
use crate::protocol::{ToolCallResult, ToolInfo};

pub struct McpTool {
    client: McpClient,
    tool: String,
    schema: ToolSchema,
    timeout: Duration,
}

impl McpTool {
    pub fn try_new(
        server: &str,
        info: ToolInfo,
        client: McpClient,
        timeout: Duration,
    ) -> Option<Arc<dyn Tool>> {
        if info.input_schema.get("type").and_then(Value::as_str) != Some("object") {
            return None;
        }
        let namespaced = namespaced_tool_name(server, &info.name);
        Some(Arc::new(Self {
            client,
            tool: info.name,
            schema: ToolSchema {
                name: ToolName::new(namespaced),
                description: info.description,
                input_schema: info.input_schema,
                output_schema: None,
            },
            timeout,
        }))
    }
}

#[must_use]
pub fn namespaced_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        self.schema.name.as_str()
    }

    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        ctx.permission
            .assert(Action::Mcp, Resource::Command(self.name().to_string()))
            .await?;
        let value = self
            .client
            .call(
                "tools/call",
                json!({ "name": self.tool, "arguments": input }),
                self.timeout,
            )
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?;
        let result: ToolCallResult =
            serde_json::from_value(value).map_err(|e| ToolError::Other(e.to_string()))?;
        if result.is_error {
            return Err(ToolError::Other(result.content.to_string()));
        }
        Ok(result.content)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};
    use tokio_util::sync::CancellationToken;
    use yaca_tool::{
        InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
        SpawnerPlane, TodoPlane, WebSearchPlane,
    };

    fn ctx_with(action: Action, mode: Mode) -> ToolCtx {
        let (permission, _rx) =
            PermissionPlane::new(PermissionRules::new(vec![Rule::new(action, "*", mode)]));
        let (interaction, _irx) = InteractionPlane::new();
        let (spawner, _srx) = SpawnerPlane::new();
        ToolCtx {
            permission,
            interaction,
            spawner,
            session: None,
            parent_session: None,
            todo: TodoPlane::default(),
            skills: SkillPlane::default(),
            websearch: WebSearchPlane::default(),
            lsp: LspPlane::default(),
            workdir: std::env::temp_dir(),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn bridge_translates_success_and_denied_permission() {
        let (client_io, server_io) = duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_io);
        let (server_read, mut server_write) = tokio::io::split(server_io);
        let client = McpClient::new(client_read, client_write);
        let tool = McpTool::try_new(
            "echo",
            ToolInfo {
                name: "ping".to_string(),
                description: "Ping".to_string(),
                input_schema: json!({ "type": "object" }),
            },
            client,
            Duration::from_secs(1),
        )
        .unwrap();
        let server = tokio::spawn(async move {
            let mut lines = BufReader::new(server_read).lines();
            let req = lines.next_line().await.unwrap().unwrap();
            let id = serde_json::from_str::<serde_json::Value>(&req).unwrap()["id"].clone();
            let response =
                json!({"jsonrpc":"2.0","id":id,"result":{"content":{"pong":true},"isError":false}});
            server_write
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        });
        let ok_ctx = ctx_with(Action::Mcp, Mode::Allow);
        let out = tool.execute(&ok_ctx, json!({})).await.unwrap();
        assert_eq!(out, json!({"pong": true}));
        server.await.unwrap();

        let denied_ctx = ctx_with(Action::Mcp, Mode::Deny);
        assert!(tool.execute(&denied_ctx, json!({})).await.is_err());
    }

    #[tokio::test]
    async fn rejects_non_object_input_schema() {
        let (client_io, _server_io) = duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_io);
        let client = McpClient::new(client_read, client_write);
        assert!(
            McpTool::try_new(
                "echo",
                ToolInfo {
                    name: "bad".to_string(),
                    description: String::new(),
                    input_schema: json!({ "type": "string" }),
                },
                client,
                Duration::from_secs(1),
            )
            .is_none()
        );
    }
}
