//! Integration tests for `hya-tool`: the merged `ask_user` batch question
//! tool (canonical) and its hidden `question` alias.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use hya_proto::SessionId;
use hya_tool::{
    InteractionPlane, LspPlane, PermissionPlane, PermissionRules, QuestionAnswer, QuestionKind,
    SkillPlane, SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry, WebSearchPlane,
    handle::ArtifactPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn ctx_with(interaction: InteractionPlane, session: SessionId) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction: interaction.for_session(session),
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir: PathBuf::from("."),
        roots: vec![PathBuf::from(".")],
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn ask_user_batch_returns_structured_answers() {
    // Given
    let session = SessionId::new();
    let (interaction, mut rx) = InteractionPlane::new();
    let ctx = ctx_with(interaction, session);
    let tool = ToolRegistry::builtins().get("ask_user").unwrap();

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

    let request = rx.recv().await.unwrap();
    assert_eq!(request.session, Some(session));
    assert_eq!(request.prompt, "Pick a color");
    assert_eq!(request.info.header, "Color");
    assert_eq!(request.questions.len(), 2);
    assert_eq!(
        request.kind,
        QuestionKind::Select {
            options: vec!["red".to_string(), "green".to_string()],
            allow_custom: true,
        }
    );
    assert!(matches!(
        request.questions[1].kind,
        QuestionKind::FreeText { default: None }
    ));
    request
        .reply
        .send_many(vec![
            QuestionAnswer::Selected(1),
            QuestionAnswer::FreeText("codex/todo".to_string()),
        ])
        .unwrap();

    // Then
    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["title"], "Asked 2 questions");
    assert_eq!(
        out["metadata"]["answers"],
        json!([
            { "question": "Pick a color", "answer": ["green"], "cancelled": false },
            {
                "question": "What should the branch be called?",
                "answer": ["codex/todo"],
                "cancelled": false
            }
        ])
    );
    assert_eq!(
        out["output"],
        "User has answered your questions: \"Pick a color\"=\"green\", \"What should the branch be called?\"=\"codex/todo\". You can now continue with the user's answers in mind."
    );
}

#[tokio::test]
async fn ask_user_supports_multiple_selection_and_allow_custom_false() {
    // Given
    let session = SessionId::new();
    let (interaction, mut rx) = InteractionPlane::new();
    let ctx = ctx_with(interaction, session);
    let tool = ToolRegistry::builtins().get("ask_user").unwrap();

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
                        "allow_custom": false,
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
    assert!(request.info.multiple);
    assert_eq!(
        request.kind,
        QuestionKind::Select {
            options: vec!["red".to_string(), "green".to_string(), "blue".to_string()],
            allow_custom: false,
        }
    );
    request
        .reply
        .send(QuestionAnswer::SelectedMany(vec![0, 2]))
        .unwrap();

    // Then
    let out = handle.await.unwrap().unwrap();
    assert_eq!(
        out["metadata"]["answers"],
        json!([{ "question": "Pick colors", "answer": ["red", "blue"], "cancelled": false }])
    );
}

#[tokio::test]
async fn ask_user_reports_cancellation_per_question() {
    // Given
    let session = SessionId::new();
    let (interaction, mut rx) = InteractionPlane::new();
    let ctx = ctx_with(interaction, session);
    let tool = ToolRegistry::builtins().get("ask_user").unwrap();

    // When
    let handle = tokio::spawn(async move {
        tool.execute(
            &ctx,
            json!({
                "questions": [
                    {
                        "question": "Ship it?",
                        "header": "confirm",
                        "options": [{ "label": "yes", "description": "proceed" }]
                    }
                ]
            }),
        )
        .await
    });

    let request = rx.recv().await.unwrap();
    request.reply.send(QuestionAnswer::Cancelled).unwrap();

    // Then
    let out = handle.await.unwrap().unwrap();
    assert_eq!(
        out["metadata"]["answers"],
        json!([{ "question": "Ship it?", "answer": [], "cancelled": true }])
    );
    assert!(out["output"].as_str().unwrap().contains("Unanswered"));
}

#[tokio::test]
async fn ask_user_propagates_plane_errors_instead_of_swallowing() {
    // Given: the host receiver is gone, so the plane is unavailable.
    let session = SessionId::new();
    let (interaction, rx) = InteractionPlane::new();
    drop(rx);
    let ctx = ctx_with(interaction, session);
    let tool = ToolRegistry::builtins().get("ask_user").unwrap();

    // When
    let result = tool
        .execute(
            &ctx,
            json!({
                "questions": [
                    {
                        "question": "Ship it?",
                        "header": "confirm",
                        "options": [{ "label": "yes", "description": "proceed" }]
                    }
                ]
            }),
        )
        .await;

    // Then
    assert!(
        matches!(&result, Err(ToolError::Other(message)) if message.contains("unavailable")),
        "plane errors must surface, got {result:?}"
    );
}

#[tokio::test]
async fn ask_user_rejects_options_without_description() {
    let session = SessionId::new();
    let (interaction, _rx) = InteractionPlane::new();
    let ctx = ctx_with(interaction, session);
    let tool = ToolRegistry::builtins().get("ask_user").unwrap();

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

    assert!(matches!(result, Err(ToolError::Input(message)) if message.contains("description")));
}

#[tokio::test]
async fn question_hidden_alias_dispatches_the_merged_tool() {
    let registry = ToolRegistry::builtins();

    // Canonical advertisement contains ask_user, never question.
    let canonical: Vec<String> = registry
        .snapshot()
        .canonical_tools()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(canonical.contains(&"ask_user".to_string()));
    assert!(!canonical.contains(&"question".to_string()));

    // The hidden alias resolves to the same merged tool.
    let aliased = registry.get("question").unwrap();
    assert_eq!(aliased.name(), "ask_user");

    // Legacy `custom` spelling still parses through the alias.
    let session = SessionId::new();
    let (interaction, mut rx) = InteractionPlane::new();
    let ctx = ctx_with(interaction, session);
    let handle = tokio::spawn(async move {
        aliased
            .execute(
                &ctx,
                json!({
                    "questions": [
                        {
                            "question": "Pick",
                            "header": "h",
                            "custom": false,
                            "options": [{ "label": "a", "description": "d" }]
                        }
                    ]
                }),
            )
            .await
    });
    let request = rx.recv().await.unwrap();
    assert_eq!(
        request.kind,
        QuestionKind::Select {
            options: vec!["a".to_string()],
            allow_custom: false,
        }
    );
    request.reply.send(QuestionAnswer::Selected(0)).unwrap();
    let out = handle.await.unwrap().unwrap();
    assert_eq!(out["title"], "Asked 1 question");
}
