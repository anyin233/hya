use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ToolCallId, ToolName, ToolSchema};
use hya_tool::{Tool, ToolCtx, ToolError};
use serde_json::Value;

use crate::host::PluginConn;
use crate::messages::ToolInfo;

pub(crate) struct PluginTool {
    conn: Arc<PluginConn>,
    tool: String,
    schema: ToolSchema,
}

impl PluginTool {
    pub(crate) fn try_new(conn: Arc<PluginConn>, info: ToolInfo) -> Option<Arc<dyn Tool>> {
        if info.input_schema.get("type").and_then(Value::as_str) != Some("object") {
            return None;
        }
        Some(Arc::new(Self {
            conn,
            tool: info.name.clone(),
            schema: ToolSchema {
                name: ToolName::new(info.name),
                description: info.description,
                input_schema: info.input_schema,
                output_schema: None,
            },
        }))
    }
}

#[async_trait]
impl Tool for PluginTool {
    fn name(&self) -> &str {
        self.schema.name.as_str()
    }

    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let session = ctx
            .session
            .ok_or_else(|| ToolError::Other("plugin tool requires a session".to_string()))?;
        let reply = self
            .conn
            .call_tool(&self.tool, session, ToolCallId::new(), input)
            .await
            .map_err(|error| ToolError::Other(error.to_string()))?;
        if !reply.ok {
            return Err(ToolError::Other(reply.output.to_string()));
        }
        Ok(reply.output)
    }
}
