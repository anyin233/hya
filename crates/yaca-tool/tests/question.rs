#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use yaca_proto::SessionId;
use yaca_tool::{
    InteractionPlane, LspPlane, Mode, PermissionPlane, PermissionRules, QuestionAnswer,
    QuestionKind, Rule, SkillPlane, SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry,
    WebSearchPlane,
};

fn ctx_with(interaction: InteractionPlane, session: SessionId) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        yaca_tool::Action::Read,
        "*",
        Mode::Allow,
    )]));
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
        workdir: PathBuf::from("."),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn question_asks_multiple_prompts_and_returns_open_code_answer_metadata() {
    // Given
    let session = SessionId::new();
    let (interaction, mut rx) = InteractionPlane::new();
    let ctx = ctx_with(interaction, session);
    let tool = ToolRegistry::builtins().get("question").unwrap();

    // When
    let handle = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({
                "questions": [
                    {
                        "question": "Pick a color",
                        "header": "Color",
                        "options": [
                            { "label": "red", "description": "Warm" },
                            { "label": "green", "description": "Calm" }
                        ]
                    },
                    {
                        "question": "What should the branch be called?",
                        "header": "Branch",
                        "options": []
                    }
                ]
            }),
        )
        .await
    });

    let first = rx.recv().await.unwrap();
    assert_eq!(first.session, Some(session));
    assert_eq!(first.prompt, "Pick a color");
    assert_eq!(
        first.kind,
        QuestionKind::Select {
            options: vec!["red".to_string(), "green".to_string()],
            allow_custom: true,
        }
    );
    first.reply.send(QuestionAnswer::Selected(1)).unwrap();

    let second = rx.recv().await.unwrap();
    assert_eq!(second.prompt, "What should the branch be called?");
    assert_eq!(
        second.kind,
        QuestionKind::FreeText {
            default: Some(String::new()),
        }
    );
    second
        .reply
        .send(QuestionAnswer::FreeText("codex/todo".to_string()))
        .unwrap();

    // Then
    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["title"], "Asked 2 questions");
    assert_eq!(
        out["metadata"]["answers"],
        json!([["green"], ["codex/todo"]])
    );
    assert_eq!(
        out["output"],
        "User has answered your questions: \"Pick a color\"=\"green\", \"What should the branch be called?\"=\"codex/todo\". You can now continue with the user's answers in mind."
    );
}

#[tokio::test]
async fn question_supports_multiple_selected_options() {
    // Given
    let session = SessionId::new();
    let (interaction, mut rx) = InteractionPlane::new();
    let ctx = ctx_with(interaction, session);
    let tool = ToolRegistry::builtins().get("question").unwrap();

    // When
    let handle = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({
                "questions": [
                    {
                        "question": "Pick colors",
                        "header": "Color",
                        "multiple": true,
                        "options": [
                            { "label": "red", "description": "Warm" },
                            { "label": "green", "description": "Calm" },
                            { "label": "blue", "description": "Cool" }
                        ]
                    }
                ]
            }),
        )
        .await
    });

    let request = rx.recv().await.unwrap();
    request
        .reply
        .send(QuestionAnswer::SelectedMany(vec![0, 2]))
        .unwrap();

    // Then
    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["metadata"]["answers"], json!([["red", "blue"]]));
    assert_eq!(
        out["output"],
        "User has answered your questions: \"Pick colors\"=\"red, blue\". You can now continue with the user's answers in mind."
    );
}

#[tokio::test]
async fn question_rejects_options_without_description() {
    // Given
    let session = SessionId::new();
    let (interaction, rx) = InteractionPlane::new();
    drop(rx);
    let ctx = ctx_with(interaction, session);
    let tool = ToolRegistry::builtins().get("question").unwrap();

    // When
    let result = tool
        .execute(
            &ctx,
            json!({
                "questions": [
                    {
                        "question": "Pick a color",
                        "header": "Color",
                        "options": [{ "label": "red" }]
                    }
                ]
            }),
        )
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Input(message)) if message.contains("description")));
}
