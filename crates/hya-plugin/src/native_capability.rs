//! A call-scoped view of host-owned services for native bundle processes.

use async_trait::async_trait;
use hya_tool::{Action, Resource, ToolCtx};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::HostCapabilityHandler;
use crate::protocol::{JsonRpcError, codes};

pub(crate) struct NativeToolCapability {
    ctx: ToolCtx,
}

impl NativeToolCapability {
    pub(crate) fn new(ctx: &ToolCtx) -> Self {
        Self { ctx: ctx.clone() }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PermissionAssert {
    action: Action,
    resource: NativeResource,
}

#[derive(Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum NativeResource {
    Tool(String),
    Path(String),
    Glob(String),
    Command(String),
    Subagent(String),
    Url(String),
    WebSearch(String),
    Skill(String),
    Any,
}

impl From<NativeResource> for Resource {
    fn from(value: NativeResource) -> Self {
        match value {
            NativeResource::Tool(value) => Self::Tool(value),
            NativeResource::Path(value) => Self::Path(value),
            NativeResource::Glob(value) => Self::Glob(value),
            NativeResource::Command(value) => Self::Command(value),
            NativeResource::Subagent(value) => Self::Subagent(value),
            NativeResource::Url(value) => Self::Url(value),
            NativeResource::WebSearch(value) => Self::WebSearch(value),
            NativeResource::Skill(value) => Self::Skill(value),
            NativeResource::Any => Self::Any,
        }
    }
}

fn rpc_error(code: i64, message: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code,
        message: message.into(),
        data: None,
    }
}

#[async_trait]
impl HostCapabilityHandler for NativeToolCapability {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, JsonRpcError> {
        if self.ctx.cancel.is_cancelled() {
            return Err(rpc_error(codes::CAPABILITY_DENIED, "tool call cancelled"));
        }
        match method {
            "context.describe" => {
                if params != json!({}) {
                    return Err(rpc_error(codes::INVALID_PARAMS, "expected empty params"));
                }
                Ok(json!({
                    "session": self.ctx.session,
                    "parent_session": self.ctx.parent_session,
                    "workdir": self.ctx.workdir,
                    "source_tool_call_id": self.ctx.operation.source_tool_call_id(),
                    "operation_id": self.ctx.operation.operation_id(),
                }))
            }
            "permission.assert" => {
                let request: PermissionAssert = serde_json::from_value(params)
                    .map_err(|error| rpc_error(codes::INVALID_PARAMS, error.to_string()))?;
                self.ctx
                    .permission
                    .assert(request.action, request.resource.into())
                    .await
                    .map_err(|error| rpc_error(codes::PERMISSION_DENIED, error.to_string()))?;
                Ok(json!({}))
            }
            _ => Err(rpc_error(
                codes::METHOD_NOT_FOUND,
                "unknown native tool capability",
            )),
        }
    }
}
