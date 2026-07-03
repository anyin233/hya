use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use hya_tool::{Action, Resource, Tool, ToolCtx, ToolError};
use serde_json::{Value, json};

use crate::client::McpClient;
use crate::protocol::{ToolCallResult, ToolInfo};

const MAX_RESOURCE_BLOB_BYTES: usize = 10 * 1024 * 1024;

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
        Ok(normalize_output(result.content))
    }
}

fn normalize_output(content: Value) -> Value {
    let items = match content {
        Value::Array(items) => items,
        other => vec![other],
    };
    let mut text = Vec::new();
    let mut attachments = Vec::new();
    for item in &items {
        format_content_item(item, &mut text, &mut attachments);
    }
    let mut output = json!({
        "title": "",
        "metadata": {},
        "output": text.join("\n\n"),
        "content": items,
    });
    if !attachments.is_empty() {
        output["attachments"] = Value::Array(attachments);
    }
    output
}

fn format_content_item(item: &Value, text: &mut Vec<String>, attachments: &mut Vec<Value>) {
    let Some(kind) = item.get("type").and_then(Value::as_str) else {
        text.push(item.to_string());
        return;
    };
    match kind {
        "text" => {
            if let Some(value) = item.get("text").and_then(Value::as_str) {
                text.push(value.to_string());
            }
        }
        "image" => {
            if let (Some(mime), Some(data)) = (
                item.get("mimeType").and_then(Value::as_str),
                item.get("data").and_then(Value::as_str),
            ) {
                attachments.push(file_attachment(mime, data, None));
            }
        }
        "resource" => format_resource_item(item, text, attachments),
        _ => text.push(item.to_string()),
    }
}

fn format_resource_item(item: &Value, text: &mut Vec<String>, attachments: &mut Vec<Value>) {
    let resource = item.get("resource").unwrap_or(item);
    let uri = resource
        .get("uri")
        .and_then(Value::as_str)
        .unwrap_or("resource");
    let mime = resource
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream");
    if let Some(value) = resource.get("text").and_then(Value::as_str) {
        text.push(value.to_string());
        return;
    }
    if let Some(blob) = resource.get("blob").and_then(Value::as_str) {
        let size = base64_size(blob);
        if !is_supported_attachment_mime(mime) {
            text.push(format!(
                "[Binary MCP resource omitted: {uri} ({mime}, {}) is not a supported attachment type]",
                format_bytes(size)
            ));
            return;
        }
        if size > MAX_RESOURCE_BLOB_BYTES {
            text.push(format!(
                "[Binary MCP resource omitted: {uri} ({mime}, {}) exceeds {}]",
                format_bytes(size),
                format_bytes(MAX_RESOURCE_BLOB_BYTES)
            ));
            return;
        }
        text.push(format!("[Binary MCP resource attached: {uri} ({mime})]"));
        attachments.push(file_attachment(mime, blob, Some(uri)));
        return;
    }
    text.push(format!(
        "[MCP resource content without text or blob: {uri}]"
    ));
}

fn file_attachment(mime: &str, data: &str, filename: Option<&str>) -> Value {
    let mut attachment = json!({
        "type": "file",
        "mime": mime,
        "url": format!("data:{mime};base64,{data}"),
    });
    if let Some(filename) = filename {
        attachment["filename"] = json!(filename);
    }
    attachment
}

fn is_supported_attachment_mime(mime: &str) -> bool {
    matches!(
        mime,
        "application/pdf" | "image/gif" | "image/jpeg" | "image/png" | "image/webp"
    )
}

fn base64_size(value: &str) -> usize {
    let trimmed = value.chars().filter(|c| !c.is_whitespace()).count();
    let padding = if value.ends_with("==") {
        2
    } else if value.ends_with('=') {
        1
    } else {
        0
    };
    (trimmed.saturating_mul(3) / 4).saturating_sub(padding)
}

fn format_bytes(value: usize) -> String {
    if value < 1024 {
        return format!("{value} B");
    }
    if value < 1024 * 1024 {
        return format!("{} KB", value.div_ceil(1024));
    }
    format!("{} MB", value.div_ceil(1024 * 1024))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use hya_tool::{
        InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
        SpawnerPlane, TodoPlane, WebSearchPlane,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};
    use tokio_util::sync::CancellationToken;

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
            formatter: hya_tool::FormatterPlane::default(),
            agents: hya_tool::AgentCatalogPlane::default(),
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
            let response = json!({
                "jsonrpc":"2.0",
                "id":id,
                "result":{
                    "content":[
                        {"type":"text","text":"pong"},
                        {"type":"image","mimeType":"image/png","data":"aGVsbG8="}
                    ],
                    "isError":false
                }
            });
            server_write
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
        });
        let ok_ctx = ctx_with(Action::Mcp, Mode::Allow);
        let out = tool.execute(&ok_ctx, json!({})).await.unwrap();
        assert_eq!(out["title"], "");
        assert_eq!(out["output"], "pong");
        assert_eq!(out["metadata"], json!({}));
        assert_eq!(
            out["content"],
            json!([
                {"type":"text","text":"pong"},
                {"type":"image","mimeType":"image/png","data":"aGVsbG8="}
            ])
        );
        assert_eq!(
            out["attachments"],
            json!([{
                "type": "file",
                "mime": "image/png",
                "url": "data:image/png;base64,aGVsbG8="
            }])
        );
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
