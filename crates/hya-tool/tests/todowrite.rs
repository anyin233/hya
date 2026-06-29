#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use hya_proto::SessionId;
use hya_tool::{
    Action, InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
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
        permission: permission.for_session(session),
        interaction,
        spawner,
        session: Some(session),
        parent_session: None,
        todo,
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        workdir: PathBuf::from("."),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn todowrite_updates_session_todos_and_reports_open_count() {
    // Given
    let session = SessionId::new();
    let todo = TodoPlane::default();
    let tool = ToolRegistry::builtins().get("todowrite").unwrap();
    let ctx = ctx_with(vec![allow(Action::TodoWrite, "*")], session, todo.clone());

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "todos": [
                    { "content": "Audit OpenCode tools", "status": "in_progress", "priority": "high" },
                    { "content": "Document parity gaps", "status": "completed", "priority": "low" }
                ]
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["title"], "1 todos");
    assert_eq!(out["metadata"]["todos"].as_array().unwrap().len(), 2);
    let stored = todo.get(session).await;
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0].content, "Audit OpenCode tools");
    assert_eq!(stored[0].status.as_str(), "in_progress");
    assert_eq!(stored[0].priority.as_str(), "high");
    assert_eq!(stored[1].status.as_str(), "completed");
}

#[tokio::test]
async fn todowrite_denied_does_not_update_session_todos() {
    // Given
    let session = SessionId::new();
    let todo = TodoPlane::default();
    let tool = ToolRegistry::builtins().get("todowrite").unwrap();
    let ctx = ctx_with(vec![deny(Action::TodoWrite, "*")], session, todo.clone());

    // When
    let result = tool
        .execute(
            &ctx,
            json!({
                "todos": [
                    { "content": "Blocked", "status": "pending", "priority": "medium" }
                ]
            }),
        )
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Permission(_))));
    assert!(todo.get(session).await.is_empty());
}

#[tokio::test]
async fn todowrite_rejects_missing_status_without_updating_session_todos() {
    // Given
    let session = SessionId::new();
    let todo = TodoPlane::default();
    let tool = ToolRegistry::builtins().get("todowrite").unwrap();
    let ctx = ctx_with(vec![allow(Action::TodoWrite, "*")], session, todo.clone());

    // When
    let result = tool
        .execute(
            &ctx,
            json!({
                "todos": [
                    { "content": "Bad state", "priority": "high" }
                ]
            }),
        )
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Input(_))));
    assert!(todo.get(session).await.is_empty());
}

#[tokio::test]
async fn todowrite_accepts_opencode_string_status_and_priority() {
    // Given
    let session = SessionId::new();
    let todo = TodoPlane::default();
    let tool = ToolRegistry::builtins().get("todowrite").unwrap();
    let schema = tool.schema().input_schema;
    let item_props = &schema["properties"]["todos"]["items"]["properties"];
    assert!(item_props["status"].get("enum").is_none());
    assert!(item_props["priority"].get("enum").is_none());
    let ctx = ctx_with(vec![allow(Action::TodoWrite, "*")], session, todo.clone());

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "todos": [
                    { "content": "Future state", "status": "blocked", "priority": "urgent" }
                ]
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["title"], "1 todos");
    let stored = todo.get(session).await;
    assert_eq!(stored[0].status.as_str(), "blocked");
    assert_eq!(stored[0].priority.as_str(), "urgent");
}
