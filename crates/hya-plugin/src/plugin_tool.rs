use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ToolCallId, ToolName, ToolSchema};
use hya_tool::{Action, Resource, Tool, ToolCtx, ToolError};
use serde_json::Value;

use crate::host::PluginConn;
use crate::messages::ToolInfo;

pub(crate) struct PluginTool {
    conn: Arc<PluginConn>,
    tool: String,
    schema: ToolSchema,
}

impl PluginTool {
    pub(crate) fn bind(conn: Arc<PluginConn>, info: ToolInfo) -> Arc<dyn Tool> {
        let name = info.name.clone();
        Self::named(conn, info, &name)
    }

    pub(crate) fn named(conn: Arc<PluginConn>, info: ToolInfo, name: &str) -> Arc<dyn Tool> {
        Arc::new(Self {
            conn,
            tool: info.name.clone(),
            schema: ToolSchema {
                name: ToolName::new(name),
                description: info.description,
                input_schema: info.input_schema,
                output_schema: None,
            },
        })
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
        let activation = self.conn.is_activation();
        if activation && ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let session = ctx.session.ok_or_else(|| {
            ToolError::Other(if activation {
                "bundle sidecar tool requires a session".to_string()
            } else {
                "plugin tool requires a session".to_string()
            })
        })?;
        if activation {
            ctx.permission
                .assert(Action::Tool, Resource::Tool(self.name().to_string()))
                .await?;
        }
        let call = if activation {
            ctx.operation.source_tool_call_id()
        } else {
            ToolCallId::new()
        };
        let request = self.conn.call_tool(ctx, &self.tool, session, call, input);
        let reply = if activation {
            tokio::select! {
                _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
                result = request => result,
            }
        } else {
            request.await
        };
        let reply = reply.map_err(|error| {
            if activation && self.conn.activation_is_lost() {
                ctx.cancel.cancel();
                ToolError::Cancelled
            } else if activation {
                ToolError::Other(format!("bundle sidecar tool call: {error}"))
            } else {
                ToolError::Other(error.to_string())
            }
        })?;
        if !reply.ok {
            return Err(ToolError::Other(reply.output.to_string()));
        }
        Ok(reply.output)
    }
}
