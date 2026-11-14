//! Engine side of [`hya_tool::NestedToolPlane`]: one model tool call running
//! the other tools of its agent's compiled view.
//!
//! Everything is captured when the calling tool's context is built, because
//! a bundle process reaches the plane from a host-capability task that has
//! none of the turn's task-local state (activation hooks included).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{AgentName, MessageId, SessionId, ToolSchema};
use hya_store::ActorClaim;
use hya_tool::{GrantScope, NestedTools, ToolCtx, ToolError};
use serde_json::Value;

use super::{SessionEngine, authorize_tool_call, file_snapshot};
use crate::TurnBinding;
use crate::hooks::HookDispatcher;
use crate::runtime_registry::CompiledResourceView;

/// What one model tool call needs to dispatch nested calls.
pub(super) struct TurnNestedTools {
    pub(super) engine: Arc<SessionEngine>,
    pub(super) binding: TurnBinding,
    pub(super) resources: Arc<CompiledResourceView>,
    pub(super) agent: AgentName,
    pub(super) session: SessionId,
    pub(super) message: MessageId,
    pub(super) actor_claim: Option<ActorClaim>,
    pub(super) external_dirs: Arc<[PathBuf]>,
    pub(super) grant_scope: GrantScope,
    pub(super) activation_hooks: Option<Arc<dyn HookDispatcher>>,
    /// Canonical name of the calling tool.
    pub(super) caller: String,
    /// The calling tool's context, with `nested` disconnected.
    pub(super) base: ToolCtx,
}

fn core_error(error: crate::error::CoreError) -> ToolError {
    ToolError::Other(error.to_string())
}

#[async_trait]
impl NestedTools for TurnNestedTools {
    fn list(&self) -> Vec<ToolSchema> {
        self.resources
            .tool_schemas()
            .into_iter()
            // A bundle tool is advertised under a view name that differs from
            // its own `name()`, so compare the resolved tool, not the schema.
            .filter(|schema| {
                self.resources
                    .resolve_tool(schema.name.as_str())
                    .is_none_or(|resolved| resolved.tool.name() != self.caller)
            })
            .collect()
    }

    async fn call(&self, name: &str, input: Value) -> Result<Value, ToolError> {
        let resolved = self
            .resources
            .resolve_tool(name)
            .ok_or_else(|| ToolError::Other(format!("unknown tool: {name}")))?;
        if resolved.tool.name() == self.caller {
            return Err(ToolError::Input(format!(
                "tool `{name}` cannot call itself"
            )));
        }
        let call = self.base.operation.source_tool_call_id();
        let permission = self
            .engine
            .tool_call_permission(
                &self.binding,
                self.session,
                &self.agent,
                &self.external_dirs,
                &self.grant_scope,
                self.activation_hooks.as_ref(),
            )
            .await
            .map_err(core_error)?;
        let permission =
            authorize_tool_call(&resolved, &input, permission, self.message, call).await?;
        let ctx = ToolCtx {
            permission,
            ..self.base.clone()
        };
        self.engine
            .validate_actor_claim(self.actor_claim.as_ref())
            .await
            .map_err(core_error)?;
        let capture = Box::pin(file_snapshot::capture_before(
            resolved.tool.name(),
            &input,
            self.binding.workdir(),
        ))
        .await;
        let result = tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => Err(ToolError::Cancelled),
            result = resolved.tool.execute(&ctx, input) => result,
        };
        Box::pin(self.engine.record_file_changes(
            self.actor_claim.as_ref(),
            self.session,
            self.message,
            call,
            capture,
        ))
        .await
        .map_err(core_error)?;
        result
    }
}
