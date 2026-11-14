//! Request-scoped host services for installed-bundle processes.
//!
//! One [`BundleCapability`] backs one capability lease: either a bundle tool
//! call (any process kind — `rust`, `bun`, or `claude`) or an `api/request`
//! bundle API request. Every operation is read-only or permission-checked
//! (`tool.call` runs a tool of the calling agent's view through the engine's
//! nested plane, authorized like a model call of that tool),
//! and the lease is bound to (connection, session-or-none, call) and revoked
//! when the reply arrives.

use std::sync::Arc;

use async_trait::async_trait;
use hya_core::{HostSessionReads, UsageScope};
use hya_proto::{SessionId, ToolCallId};
use hya_tool::{Action, Resource, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::client::HostCapabilityHandler;
use crate::protocol::{JsonRpcError, codes};

/// What a lease was minted for.
enum Grant {
    /// A bundle tool call: its full tool context (permission plane, workdir).
    ToolCall(Box<ToolCtx>),
    /// A bundle API request: bound to its session for a session-scoped
    /// endpoint, to no session for a global one.
    Api {
        api: String,
        session: Option<SessionId>,
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

    /// Capability for one `api/request` (the synthetic `call` is set by the
    /// client when it mints the lease; the grant only reports it).
    pub(crate) fn api(
        api: &str,
        session: Option<SessionId>,
        call: ToolCallId,
        reads: Option<Arc<dyn HostSessionReads>>,
    ) -> Self {
        Self {
            grant: Grant::Api {
                api: api.to_string(),
                session,
                call,
            },
            reads,
        }
    }

    async fn session_usage(&self, params: Value) -> Result<Value, JsonRpcError> {
        if let Grant::Api { session: None, .. } = &self.grant {
            return Err(rpc_error(
                codes::CAPABILITY_DENIED,
                "session.usage is unavailable to a global API request (it has no session)",
            ));
        }
        let request: SessionUsageParams = serde_json::from_value(params)
            .map_err(|error| rpc_error(codes::INVALID_PARAMS, error.to_string()))?;
        let session = match &self.grant {
            Grant::ToolCall(ctx) => ctx.session.ok_or_else(|| {
                rpc_error(
                    codes::CAPABILITY_DENIED,
                    "session.usage requires a session-bound tool call",
                )
            })?,
            Grant::Api { session, .. } => {
                if request.scope == UsageScope::Root {
                    return Err(rpc_error(
                        codes::INVALID_PARAMS,
                        "an API request may read only its session: scope `root` is not available",
                    ));
                }
                session.ok_or_else(|| {
                    rpc_error(
                        codes::CAPABILITY_DENIED,
                        "session.usage is unavailable to a global API request",
                    )
                })?
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

/// Largest `tool.call` result, as serialized JSON, sent back to the process.
const MAX_TOOL_CALL_RESULT_BYTES: usize = 768 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCallParams {
    name: String,
    input: serde_json::Map<String, Value>,
}

async fn nested_tool_call(ctx: &ToolCtx, params: Value) -> Result<Value, JsonRpcError> {
    let request: ToolCallParams = serde_json::from_value(params)
        .map_err(|error| rpc_error(codes::INVALID_PARAMS, error.to_string()))?;
    let output = ctx
        .nested
        .call(&request.name, Value::Object(request.input))
        .await
        .map_err(|error| match error {
            ToolError::Permission(error) => rpc_error(codes::PERMISSION_DENIED, error),
            error => rpc_error(codes::TOOL_FAILED, error),
        })?;
    let result = json!({ "output": output });
    let size = serde_json::to_vec(&result)
        .map_err(|error| rpc_error(codes::INTERNAL_ERROR, error))?
        .len();
    if size > MAX_TOOL_CALL_RESULT_BYTES {
        return Err(rpc_error(
            codes::TOOL_FAILED,
            format!(
                "result of `{}` is too large ({size} bytes, limit {MAX_TOOL_CALL_RESULT_BYTES}); narrow the call",
                request.name
            ),
        ));
    }
    Ok(result)
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
                    Grant::Api { api, session, call } => {
                        let mut described = json!({
                            "request": "api",
                            "api": api,
                            "scope": if session.is_some() { "session" } else { "global" },
                            "call": call,
                        });
                        if let (Some(session), Some(object)) = (session, described.as_object_mut())
                        {
                            object.insert("session".to_string(), json!(session));
                        }
                        described
                    }
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
            "workspace.state" => {
                let Grant::ToolCall(ctx) = &self.grant else {
                    return Err(rpc_error(
                        codes::CAPABILITY_DENIED,
                        "workspace.state is available only to tool calls",
                    ));
                };
                if params != json!({}) {
                    return Err(rpc_error(codes::INVALID_PARAMS, "expected empty params"));
                }
                let state = hya_core::engine::workspace_state(&ctx.workdir).await;
                Ok(json!({ "state": state }))
            }
            "tools.list" => {
                let Grant::ToolCall(ctx) = &self.grant else {
                    return Err(rpc_error(
                        codes::CAPABILITY_DENIED,
                        "tools.list is available only to tool calls",
                    ));
                };
                if params != json!({}) {
                    return Err(rpc_error(codes::INVALID_PARAMS, "expected empty params"));
                }
                let tools: Vec<Value> = ctx
                    .nested
                    .list()
                    .into_iter()
                    .map(|schema| {
                        json!({
                            "name": schema.name,
                            "description": schema.description,
                            "input_schema": schema.input_schema,
                        })
                    })
                    .collect();
                Ok(json!({ "tools": tools }))
            }
            "tool.call" => {
                let Grant::ToolCall(ctx) = &self.grant else {
                    return Err(rpc_error(
                        codes::CAPABILITY_DENIED,
                        "tool.call is available only to tool calls",
                    ));
                };
                nested_tool_call(ctx, params).await
            }
            _ => Err(rpc_error(
                codes::METHOD_NOT_FOUND,
                "unknown host capability",
            )),
        }
    }
}
