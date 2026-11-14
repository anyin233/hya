//! Nested tool calls: a tool invoking the other tools of its caller's view.
//!
//! The engine connects a [`NestedToolPlane`] for every model tool call. A
//! nested call is authorized like a model call of that tool and its file
//! changes are recorded against the calling tool call; it emits no tool events
//! of its own and runs with a disconnected plane, so nesting is one level deep.

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::ToolSchema;
use serde_json::Value;

use crate::tool::ToolError;

/// Host side of [`NestedToolPlane`], implemented by the engine.
#[async_trait]
pub trait NestedTools: Send + Sync {
    /// Schemas of the tools the calling tool may invoke.
    fn list(&self) -> Vec<ToolSchema>;
    /// Run one tool as part of the calling tool call.
    async fn call(&self, name: &str, input: Value) -> Result<Value, ToolError>;
}

/// Call-scoped access to the calling agent's other tools.
#[derive(Clone)]
pub struct NestedToolPlane {
    tools: Option<Arc<dyn NestedTools>>,
}

impl NestedToolPlane {
    /// A plane that lists nothing and refuses every call.
    #[must_use]
    pub fn disconnected() -> Self {
        Self { tools: None }
    }

    /// A plane backed by `tools`.
    #[must_use]
    pub fn new(tools: Arc<dyn NestedTools>) -> Self {
        Self { tools: Some(tools) }
    }

    /// Whether nested calls are available.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.tools.is_some()
    }

    /// Schemas of the callable tools; empty when disconnected.
    #[must_use]
    pub fn list(&self) -> Vec<ToolSchema> {
        self.tools
            .as_ref()
            .map(|tools| tools.list())
            .unwrap_or_default()
    }

    /// Run `name` with `input` as part of the calling tool call.
    ///
    /// # Errors
    ///
    /// [`ToolError::Other`] when disconnected; otherwise the nested tool's error.
    pub async fn call(&self, name: &str, input: Value) -> Result<Value, ToolError> {
        let tools = self
            .tools
            .as_ref()
            .ok_or_else(|| ToolError::Other("nested tool calls are unavailable".to_string()))?;
        tools.call(name, input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn disconnected_plane_lists_nothing_and_refuses_calls() {
        let plane = NestedToolPlane::disconnected();
        assert!(!plane.is_connected());
        assert!(plane.list().is_empty());
        let error = plane.call("read", json!({})).await.err();
        assert!(matches!(error, Some(ToolError::Other(_))), "{error:?}");
    }
}
