//! Integration tests for `hya-tool`: the scheme extension registry and the
//! read/write dispatch it feeds.
//!
//! The registry owns `scheme → { owner, canonical tool, writable }` bindings
//! for external URI schemes. Dispatch is exercised through the same decorators
//! the runtime's compiled views install around `read` and `write`: a registered
//! scheme routes to its owning tool with `{"reference": "<handle>"}`, an
//! unregistered scheme behaves exactly as before, and only `writable`
//! schemes accept write dispatch.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_proto::{ToolCallId, ToolSchema};
use hya_tool::handle::{
    ArtifactPlane, SchemeBinding, SchemeDispatch, SchemeHandler, SchemeReadTool, SchemeRegistry,
    SchemeRegistryError, SchemeWriteTool,
};
use hya_tool::{
    Action, FormatterPlane, InteractionPlane, LifecyclePlane, LspPlane, MailboxPlane, Mode,
    PermissionPlane, PermissionRules, Rule, SkillPlane, SpawnerPlane, TodoPlane, Tool, ToolCtx,
    ToolError, ToolOperation, ToolRegistry, ToolResultPolicy, WebSearchPlane, WorkflowPlane,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn ctx(workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "*",
        Mode::Allow,
    )]));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        workflows: WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: ToolOperation::from_tool_call(ToolCallId::new()),
        mailbox: MailboxPlane::disconnected(),
        lifecycle: LifecyclePlane::disconnected(),
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        agents: Default::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: FormatterPlane::default(),
        workdir,
        cancel: CancellationToken::new(),
    }
}

fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hya-scheme-dispatch-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A tool that records every reference argument it is dispatched with.
struct RecordingTool {
    name: String,
    body: String,
    seen: Mutex<Vec<String>>,
}

impl RecordingTool {
    fn new(name: &str, body: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            body: body.to_string(),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn references(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: hya_proto::ToolName::new(self.name.clone()),
            description: "records dispatch references".to_string(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
        }
    }

    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Coding
    }

    async fn execute(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if let Some(reference) = input.get("reference").and_then(Value::as_str) {
            self.seen.lock().unwrap().push(reference.to_string());
        }
        Ok(json!({ "output": self.body }))
    }
}

fn dispatch_with(handlers: Vec<(&str, SchemeBinding, Arc<dyn Tool>)>) -> SchemeDispatch {
    SchemeDispatch::new(
        handlers
            .into_iter()
            .map(|(scheme, binding, tool)| (scheme.to_string(), SchemeHandler::new(binding, tool)))
            .collect(),
    )
}

#[test]
fn registry_registers_and_resolves_external_schemes() {
    let mut registry = SchemeRegistry::new();
    registry
        .register(
            "db",
            SchemeBinding::new("plugin:vecdb", "vecdb__query", false),
        )
        .expect("external scheme must register");
    assert_eq!(
        registry.resolve("db").map(SchemeBinding::canonical_tool),
        Some("vecdb__query")
    );
    assert_eq!(
        registry.resolve("db").map(SchemeBinding::owner),
        Some("plugin:vecdb")
    );
    assert!(registry.resolve("nosuch").is_none());
}

#[test]
fn registry_rejects_protected_schemes_naming_them() {
    for scheme in ["artifact", "skill", "local"] {
        let error = SchemeRegistry::new()
            .register(scheme, SchemeBinding::new("plugin:x", "x__y", false))
            .expect_err("internal scheme registration must be a hard error");
        assert_eq!(
            error,
            SchemeRegistryError::ProtectedScheme(scheme.to_string())
        );
        assert!(
            error.to_string().contains(scheme),
            "error must name the protected scheme: {error}"
        );
    }
}

#[test]
fn registry_rejects_conflicting_reregistration_naming_current_owner() {
    let mut registry = SchemeRegistry::new();
    registry
        .register(
            "db",
            SchemeBinding::new("plugin:first", "first__query", false),
        )
        .expect("first registration must succeed");
    let error = registry
        .register(
            "db",
            SchemeBinding::new("plugin:second", "second__query", true),
        )
        .expect_err("conflicting re-registration must be rejected");
    assert!(
        error.to_string().contains("plugin:first"),
        "conflict must name the current owner: {error}"
    );
    assert_eq!(
        registry.resolve("db").map(SchemeBinding::owner),
        Some("plugin:first"),
        "the incumbent binding must stay authoritative"
    );
}

#[test]
fn registry_validates_scheme_tokens() {
    assert!(SchemeRegistry::is_valid_scheme_token("db"));
    assert!(SchemeRegistry::is_valid_scheme_token("vec-db_2"));
    assert!(!SchemeRegistry::is_valid_scheme_token("d"));
    assert!(!SchemeRegistry::is_valid_scheme_token(""));
    assert!(!SchemeRegistry::is_valid_scheme_token("has__sep"));
    assert!(!SchemeRegistry::is_valid_scheme_token("no space"));
    assert!(!SchemeRegistry::is_valid_scheme_token("no.dot"));
}

#[tokio::test]
async fn read_dispatches_registered_scheme_to_owner_with_reference() {
    let owner = RecordingTool::new("vecdb__query", "rows from db");
    let dispatch = dispatch_with(vec![(
        "db",
        SchemeBinding::new("plugin:vecdb", "vecdb__query", false),
        owner.clone(),
    )]);
    let inner = ToolRegistry::builtins()
        .resolve("read")
        .expect("builtins carry read");
    let read = SchemeReadTool::new(inner.tool, dispatch);

    let ctx = ctx(tempdir());
    let result = read
        .execute(&ctx, json!({ "path": "db://x/y?head=2" }))
        .await
        .expect("registered scheme must dispatch");
    assert_eq!(
        result.get("output").and_then(Value::as_str),
        Some("rows from db"),
        "the owner tool's output body is used directly"
    );
    assert_eq!(
        owner.references(),
        vec!["db://x/y?head=2".to_string()],
        "the owner receives the full handle text as its reference argument"
    );
}

#[tokio::test]
async fn write_rejects_non_writable_scheme() {
    let owner = RecordingTool::new("vecdb__query", "rows");
    let dispatch = dispatch_with(vec![(
        "db",
        SchemeBinding::new("plugin:vecdb", "vecdb__query", false),
        owner.clone(),
    )]);
    let inner = ToolRegistry::builtins()
        .resolve("write")
        .expect("builtins carry write");
    let write = SchemeWriteTool::new(inner.tool, dispatch);

    let ctx = ctx(tempdir());
    let error = write
        .execute(&ctx, json!({ "path": "db://x/y", "content": "row" }))
        .await
        .expect_err("a read-only scheme must reject write dispatch");
    assert!(
        error.to_string().contains("db:// is read-only"),
        "non-writable write must fail with the NotWritable-style error: {error}"
    );
    assert!(
        owner.references().is_empty(),
        "a rejected write must never reach the owner tool"
    );
}

#[tokio::test]
async fn write_dispatches_writable_scheme_to_owner() {
    let owner = RecordingTool::new("kv__put", "stored");
    let dispatch = dispatch_with(vec![(
        "kv",
        SchemeBinding::new("plugin:kv", "kv__put", true),
        owner.clone(),
    )]);
    let inner = ToolRegistry::builtins()
        .resolve("write")
        .expect("builtins carry write");
    let write = SchemeWriteTool::new(inner.tool, dispatch);

    let ctx = ctx(tempdir());
    let result = write
        .execute(&ctx, json!({ "path": "kv://bucket/key", "content": "row" }))
        .await
        .expect("writable scheme must dispatch");
    assert_eq!(result.get("output").and_then(Value::as_str), Some("stored"));
    assert_eq!(owner.references(), vec!["kv://bucket/key".to_string()]);
}

#[tokio::test]
async fn unregistered_and_internal_schemes_pass_through_to_inner_tool() {
    let owner = RecordingTool::new("vecdb__query", "rows");
    let dispatch = dispatch_with(vec![(
        "db",
        SchemeBinding::new("plugin:vecdb", "vecdb__query", false),
        owner.clone(),
    )]);
    let inner = ToolRegistry::builtins()
        .resolve("read")
        .expect("builtins carry read");
    let read = SchemeReadTool::new(inner.tool, dispatch);

    let workdir = tempdir();
    std::fs::write(workdir.join("plain.txt"), "ordinary").unwrap();
    let ctx = ctx(workdir);

    let error = read
        .execute(&ctx, json!({ "path": "unknown://x/y" }))
        .await
        .expect_err("unregistered external scheme keeps the existing behavior");
    assert!(
        error.to_string().contains("unknown handle scheme"),
        "pass-through must preserve the historical unknown-scheme error: {error}"
    );
    assert!(owner.references().is_empty());

    let result = read
        .execute(&ctx, json!({ "path": "plain.txt" }))
        .await
        .expect("ordinary path must flow to the inner read untouched");
    assert!(
        result.to_string().contains("ordinary"),
        "ordinary path is presented by the inner read: {result}"
    );
    assert!(owner.references().is_empty());
}
