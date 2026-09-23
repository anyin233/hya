//! Request-scoped host services for installed-bundle processes.
//!
//! One [`BundleCapability`] backs one capability lease: either a bundle tool
//! call (any process kind — `rust`, `bun`, or `claude`) or a `view/get` view
//! request. Every operation is read-only or permission-checked, and the lease
//! is bound to (connection, session, call) and revoked when the reply arrives.

use std::sync::Arc;

use async_trait::async_trait;
use hya_core::{HostSessionReads, UsageScope};
use hya_proto::{SessionId, ToolCallId};
use hya_tool::{Action, Resource, ToolCtx};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::HostCapabilityHandler;
use crate::protocol::{JsonRpcError, codes};

/// What a lease was minted for.
enum Grant {
    /// A bundle tool call: its full tool context (permission plane, workdir).
    ToolCall(Box<ToolCtx>),
    /// A read-only view request for one session.
    View {
        session: SessionId,
        view: String,
        call: ToolCallId,
    },
}

pub(crate) struct BundleCapability {
    grant: Grant,
    reads: Option<Arc<dyn HostSessionReads>>,
}

impl BundleCapability {
    /// Capability for one bundle tool call.
    pub(crate) fn tool_call(ctx: &ToolCtx, reads: Option<Arc<dyn HostSessionReads>>) -> Self {
        Self {
            grant: Grant::ToolCall(Box::new(ctx.clone())),
            reads,
        }
    }

    /// Capability for one `view/get` request.
    pub(crate) fn view(
        session: SessionId,
        view: &str,
        call: ToolCallId,
        reads: Option<Arc<dyn HostSessionReads>>,
    ) -> Self {
        Self {
            grant: Grant::View {
                session,
                view: view.to_string(),
                call,
            },
            reads,
        }
    }

    async fn session_usage(&self, params: Value) -> Result<Value, JsonRpcError> {
        let request: SessionUsageParams = serde_json::from_value(params)
            .map_err(|error| rpc_error(codes::INVALID_PARAMS, error.to_string()))?;
        let session = match &self.grant {
            Grant::ToolCall(ctx) => ctx.session.ok_or_else(|| {
                rpc_error(
                    codes::CAPABILITY_DENIED,
                    "session.usage requires a session-bound tool call",
                )
            })?,
            Grant::View { session, .. } => {
                if request.scope == UsageScope::Root {
                    return Err(rpc_error(
                        codes::INVALID_PARAMS,
                        "a view request may read only its session: scope `root` is not available",
                    ));
                }
                *session
            }
        };
        let reads = self.reads.as_ref().ok_or_else(|| {
            rpc_error(
                codes::CAPABILITY_DENIED,
                "session.usage is unavailable in this host",
            )
        })?;
        let report = reads
            .session_usage(session, request.scope)
            .await
            .map_err(|error| rpc_error(codes::INTERNAL_ERROR, error.to_string()))?;
        serde_json::to_value(report).map_err(|error| rpc_error(codes::INTERNAL_ERROR, error))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionUsageParams {
    #[serde(default)]
    scope: UsageScope,
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

fn rpc_error(code: i64, message: impl ToString) -> JsonRpcError {
    JsonRpcError {
        code,
        message: message.to_string(),
        data: None,
    }
}

#[async_trait]
impl HostCapabilityHandler for BundleCapability {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, JsonRpcError> {
        if let Grant::ToolCall(ctx) = &self.grant
            && ctx.cancel.is_cancelled()
        {
            return Err(rpc_error(codes::CAPABILITY_DENIED, "tool call cancelled"));
        }
        match method {
            "context.describe" => {
                if params != json!({}) {
                    return Err(rpc_error(codes::INVALID_PARAMS, "expected empty params"));
                }
                Ok(match &self.grant {
                    Grant::ToolCall(ctx) => json!({
                        "request": "tool_call",
                        "session": ctx.session,
                        "parent_session": ctx.parent_session,
                        "workdir": ctx.workdir,
                        "source_tool_call_id": ctx.operation.source_tool_call_id(),
                        "operation_id": ctx.operation.operation_id(),
                    }),
                    Grant::View {
                        session,
                        view,
                        call,
                    } => json!({
                        "request": "view",
                        "session": session,
                        "view": view,
                        "call": call,
                    }),
                })
            }
            "permission.assert" => {
                let Grant::ToolCall(ctx) = &self.grant else {
                    return Err(rpc_error(
                        codes::CAPABILITY_DENIED,
                        "permission.assert is available only to tool calls",
                    ));
                };
                let request: PermissionAssert = serde_json::from_value(params)
                    .map_err(|error| rpc_error(codes::INVALID_PARAMS, error.to_string()))?;
                ctx.permission
                    .assert(request.action, request.resource.into())
                    .await
                    .map_err(|error| rpc_error(codes::PERMISSION_DENIED, error.to_string()))?;
                Ok(json!({}))
            }
            "session.usage" => self.session_usage(params).await,
            _ => Err(rpc_error(
                codes::METHOD_NOT_FOUND,
                "unknown host capability",
            )),
        }
    }
}
