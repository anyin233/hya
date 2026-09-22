//! Integration tests for `hya-tool`: the `todo__` namespaced tool group
//! (`todo__read`, `todo__update_status`, `todo__update_content`).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use hya_proto::SessionId;
use hya_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
    handle::ArtifactPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

fn deny(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Deny)
}

fn ctx_with(rules: Vec<Rule>, session: SessionId, todo: TodoPlane) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission: permission.for_session(session),
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(session),
        parent_session: None,
        todo,
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir: PathBuf::from("."),
        cancel: CancellationToken::new(),
    }
}

#[test]
fn todo_tools_are_loaded_from_the_todo_bundle() {
    let registry = ToolRegistry::builtins();
    for name in ["todo__read", "todo__update_status", "todo__update_content"] {
        assert_eq!(registry.builtin_bundle_origin(name), Some("hya/todo-tools"));
    }
    assert!(
        !include_str!("../src/todo.rs").contains("impl Tool for"),
        "the interface crate must not keep the TODO implementations"
    );
}

#[tokio::test]
async fn update_content_add_assigns_stable_sequential_ids() {
    let session = SessionId::new();
    let todo = TodoPlane::default();
    let tool = ToolRegistry::builtins()
        .get("todo__update_content")
        .unwrap();

    let out = tool
        .execute(
            &ctx_with(vec![allow(Action::TodoWrite, "*")], session, todo.clone()),
            json!({ "operations": [
                { "op": "add", "content": "first task" },
                { "op": "add", "content": "second task" }
            ]}),
        )
        .await
        .unwrap();
    let items = out["metadata"]["todos"].as_array().unwrap().clone();
    assert_eq!(items[0]["id"], "1");
    assert_eq!(items[0]["content"], "first task");
    assert_eq!(items[0]["status"], "pending");
    assert_eq!(items[1]["id"], "2");

    // A later add continues the sequence; a removal never reuses ids.
    let out = tool
        .execute(
            &ctx_with(vec![allow(Action::TodoWrite, "*")], session, todo.clone()),
            json!({ "operations": [
                { "op": "remove", "id": "1" },
                { "op": "add", "content": "third task" }
            ]}),
        )
        .await
        .unwrap();
    let items = out["metadata"]["todos"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "2");
    assert_eq!(items[1]["id"], "3");
    assert_eq!(out["metadata"]["addedIds"], json!(["3"]));
}

#[tokio::test]
async fn update_content_edit_and_batch_atomicity() {
    let session = SessionId::new();
    let todo = TodoPlane::default();
    let registry = ToolRegistry::builtins();
    let tool = registry.get("todo__update_content").unwrap();
    let ctx = || ctx_with(vec![allow(Action::TodoWrite, "*")], session, todo.clone());

    tool.execute(
        &ctx(),
        json!({ "operations": [{ "op": "add", "content": "original" }] }),
    )
    .await
    .unwrap();

    // Edit rewrites content in place, keeping id and status.
    let out = tool
        .execute(
            &ctx(),
            json!({ "operations": [{ "op": "edit", "id": "1", "content": "rewritten" }] }),
        )
        .await
        .unwrap();
    assert_eq!(out["metadata"]["todos"][0]["content"], "rewritten");
    assert_eq!(out["metadata"]["todos"][0]["id"], "1");

    // A batch referencing an unknown id fails whole: no partial application.
    let err = tool
        .execute(
            &ctx(),
            json!({ "operations": [
                { "op": "edit", "id": "1", "content": "should not persist" },
                { "op": "remove", "id": "99" }
            ]}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ToolError::Input(m) if m.contains("99")),
        "{err:?}"
    );
    let read = registry.get("todo__read").unwrap();
    let out = read.execute(&ctx(), json!({})).await.unwrap();
    assert_eq!(out["metadata"]["todos"][0]["content"], "rewritten");
}

#[tokio::test]
async fn update_status_batch_validates_ids_and_statuses() {
    let session = SessionId::new();
    let todo = TodoPlane::default();
    let registry = ToolRegistry::builtins();
    let content = registry.get("todo__update_content").unwrap();
    let status = registry.get("todo__update_status").unwrap();
    let ctx = || ctx_with(vec![allow(Action::TodoWrite, "*")], session, todo.clone());

    content
        .execute(
            &ctx(),
            json!({ "operations": [
                { "op": "add", "content": "a" },
                { "op": "add", "content": "b" }
            ]}),
        )
        .await
        .unwrap();

    let out = status
        .execute(
            &ctx(),
            json!({ "updates": [
                { "id": "1", "status": "in_progress" },
                { "id": "2", "status": "blocked" }
            ]}),
        )
        .await
        .unwrap();
    assert_eq!(out["metadata"]["todos"][0]["status"], "in_progress");
    assert_eq!(out["metadata"]["todos"][1]["status"], "blocked");

    // Unknown status spelling is an input error.
    let err = status
        .execute(
            &ctx(),
            json!({ "updates": [{ "id": "1", "status": "done" }] }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Input(_)), "{err:?}");

    // Unknown id reports the valid ids.
    let err = status
        .execute(
            &ctx(),
            json!({ "updates": [{ "id": "7", "status": "pending" }] }),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ToolError::Input(m) if m.contains('1') && m.contains('2')),
        "{err:?}"
    );
}

#[tokio::test]
async fn read_reports_snapshot_without_permission_grant() {
    let session = SessionId::new();
    let todo = TodoPlane::default();
    let registry = ToolRegistry::builtins();
    let content = registry.get("todo__update_content").unwrap();
    let read = registry.get("todo__read").unwrap();

    // No rules at all: read stays allowed, writes are denied.
    let empty_ctx = ctx_with(Vec::new(), session, todo.clone());
    let out = read.execute(&empty_ctx, json!({})).await.unwrap();
    assert_eq!(out["metadata"]["todos"], json!([]));

    content
        .execute(
            &ctx_with(vec![allow(Action::TodoWrite, "*")], session, todo.clone()),
            json!({ "operations": [
                { "op": "add", "content": "only" },
                { "op": "add", "content": "second" }
            ]}),
        )
        .await
        .unwrap();
    content
        .execute(
            &ctx_with(vec![allow(Action::TodoWrite, "*")], session, todo.clone()),
            json!({ "operations": [{ "op": "remove", "id": "2" }] }),
        )
        .await
        .unwrap();
    let out = read.execute(&empty_ctx, json!({})).await.unwrap();
    assert_eq!(out["title"], "1 todo");
    assert_eq!(out["metadata"]["todos"].as_array().unwrap().len(), 1);

    // Explicit deny rejects the write tools.
    let denied = ctx_with(vec![deny(Action::TodoWrite, "*")], session, todo);
    let err = registry
        .get("todo__update_status")
        .unwrap()
        .execute(
            &denied,
            json!({ "updates": [{ "id": "1", "status": "completed" }] }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Permission(_)), "{err:?}");
}

#[test]
fn todo_group_registered_under_namespace_without_legacy_names() {
    let registry = ToolRegistry::builtins();
    let canonical: Vec<String> = registry
        .snapshot()
        .canonical_tools()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for expected in ["todo__read", "todo__update_status", "todo__update_content"] {
        assert!(
            canonical.contains(&expected.to_string()),
            "{expected} missing"
        );
    }
    assert!(!canonical.contains(&"todowrite".to_string()));
    assert!(registry.get("todowrite").is_none());
    assert!(registry.get("todo").is_none());
    assert_eq!(canonical.len(), 27);
}

#[tokio::test]
async fn todo_tools_require_a_session() {
    let session = SessionId::new();
    let todo = TodoPlane::default();
    let mut ctx = ctx_with(vec![allow(Action::TodoWrite, "*")], session, todo);
    ctx.session = None;
    let tool = ToolRegistry::builtins()
        .get("todo__update_content")
        .unwrap();
    let err = tool
        .execute(
            &ctx,
            json!({ "operations": [{ "op": "add", "content": "x" }] }),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ToolError::Other(m) if m.contains("session")),
        "{err:?}"
    );
}
