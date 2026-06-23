#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use yaca_proto::SessionId;
use yaca_tool::{
    InteractionPlane, LspPlane, PermissionPlane, PermissionRules, QuestionAnswer, QuestionKind,
    SkillPlane, SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
};

fn ctx_with(interaction: InteractionPlane, session: SessionId) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        permission: permission.for_session(session),
        interaction: interaction.for_session(session),
        spawner,
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: yaca_tool::FormatterPlane::default(),
        workdir: PathBuf::from("."),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn plan_exit_asks_for_approval_and_reports_open_code_success_shape() {
    // Given
    let session = SessionId::new();
    let (interaction, mut rx) = InteractionPlane::new();
    let ctx = ctx_with(interaction, session);
    let tool = ToolRegistry::builtins().get("plan_exit").unwrap();

    // When
    let handle = tokio::spawn(async move { tool.execute(&ctx, json!({})).await });
    let request = rx.recv().await.unwrap();
    assert_eq!(request.session, Some(session));
    assert!(
        request
            .prompt
            .starts_with("Plan at current plan is complete.")
    );
    assert_eq!(
        request.kind,
        QuestionKind::Select {
            options: vec!["Yes".to_string(), "No".to_string()],
            allow_custom: false,
        }
    );
    request.reply.send(QuestionAnswer::Selected(0)).unwrap();

    // Then
    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["title"], "Switching to build agent");
    assert_eq!(
        out["output"],
        "User approved switching to build agent. Wait for further instructions."
    );
    assert_eq!(out["metadata"], json!({}));
}

#[tokio::test]
async fn plan_exit_rejects_when_user_declines_switching_to_build_agent() {
    // Given
    let session = SessionId::new();
    let (interaction, mut rx) = InteractionPlane::new();
    let ctx = ctx_with(interaction, session);
    let tool = ToolRegistry::builtins().get("plan_exit").unwrap();

    // When
    let handle = tokio::spawn(async move { tool.execute(&ctx, json!({})).await });
    let request = rx.recv().await.unwrap();
    request.reply.send(QuestionAnswer::Selected(1)).unwrap();

    // Then
    let result = handle.await.unwrap();
    assert!(matches!(result, Err(ToolError::Other(_))));
}
