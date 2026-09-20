//! Namespaced tool names (`namespace__local`): composition, parsing, and the
//! namespaced registration path on [`ToolRegistry`].

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use hya_tool::{
    DuplicateName, NamespacedRegisterError, Tool, ToolCtx, ToolError, ToolRegistry, namespace_of,
    namespaced_name,
};
use serde_json::{Value, json};

struct Probe(&'static str);

#[async_trait]
impl Tool for Probe {
    fn name(&self) -> &str {
        self.0
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.0),
            description: "probe".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }
    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Ok(json!({ "probe": self.0 }))
    }
}

#[test]
fn namespaced_name_composes_and_round_trips() {
    let full = namespaced_name("todo", "update_status").unwrap();
    assert_eq!(full, "todo__update_status");
    assert_eq!(namespace_of(&full), Some("todo"));
    assert_eq!(namespace_of("read"), None);
    // First-segment rule: MCP-style nested names still resolve a namespace.
    assert_eq!(namespace_of("mcp__github__read"), Some("mcp"));
    // Degenerate spellings are not namespaced.
    assert_eq!(namespace_of("__read"), None);
    assert_eq!(namespace_of("todo__"), None);
}

#[test]
fn namespaced_name_rejects_invalid_tokens() {
    for (namespace, local) in [
        ("", "read"),
        ("todo", ""),
        ("to do", "read"),
        ("todo:", "read"),
        ("todo", "re/ad"),
        ("todo__x", "read"),
        ("todo", "read__status"),
    ] {
        assert!(
            namespaced_name(namespace, local).is_err(),
            "expected rejection for ns={namespace:?} local={local:?}"
        );
    }
    // Single underscores and dashes inside tokens stay legal.
    assert_eq!(
        namespaced_name("my_tools", "update-status").unwrap(),
        "my_tools__update-status"
    );
}

#[tokio::test]
async fn register_namespaced_allows_same_local_name_across_namespaces() {
    let registry = ToolRegistry::builtins();
    registry
        .register_namespaced("alpha", "read", Arc::new(Probe("alpha__read")))
        .unwrap();
    registry
        .register_namespaced("beta", "read", Arc::new(Probe("beta__read")))
        .unwrap();

    let alpha = registry.get("alpha__read").unwrap();
    let beta = registry.get("beta__read").unwrap();
    assert_eq!(alpha.name(), "alpha__read");
    assert_eq!(beta.name(), "beta__read");

    // Duplicate canonical name (same namespace + local) is rejected.
    let err = registry
        .register_namespaced("alpha", "read", Arc::new(Probe("alpha__read")))
        .unwrap_err();
    assert!(
        matches!(err, NamespacedRegisterError::Duplicate(DuplicateName { ref name }) if name == "alpha__read"),
        "expected duplicate rejection, got {err:?}"
    );
}

#[tokio::test]
async fn register_namespaced_rejects_name_mismatch_and_invalid_tokens() {
    let registry = ToolRegistry::builtins();
    // The tool's own name must match the composed canonical name so the
    // advertised schema and registry key cannot drift apart.
    let mismatch = registry
        .register_namespaced("alpha", "read", Arc::new(Probe("other_name")))
        .unwrap_err();
    assert!(matches!(
        &mismatch,
        NamespacedRegisterError::NameMismatch { expected, .. } if expected == "alpha__read"
    ));

    let invalid = registry
        .register_namespaced("bad ns", "read", Arc::new(Probe("x")))
        .unwrap_err();
    assert!(matches!(invalid, NamespacedRegisterError::Invalid(_)));

    // Nothing was registered by the failed attempts.
    assert!(registry.get("alpha__read").is_none());
    assert!(registry.get("x").is_none());
}

#[test]
fn builtin_registry_untouched_by_namespace_mechanism() {
    let registry = ToolRegistry::builtins();
    assert!(registry.get("read").is_some());
    assert!(registry.get("todo__read").is_none());
    assert_eq!(namespace_of("read"), None);
}
