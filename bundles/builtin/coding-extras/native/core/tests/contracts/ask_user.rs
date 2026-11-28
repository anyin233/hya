//! Interactive question behavior.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use hya_proto::SessionId;
use hya_tool::{InteractionPlane, QuestionAnswer, QuestionKind, ToolError};
use serde_json::json;

use crate::common::ctx_with;

#[tokio::test]
async fn ask_user_batch_multiselect_cancel_errors() {
    let ask = crate::coding_extras_registry().get("ask_user").unwrap();

    // 1. Batch select + freetext.
    let session = SessionId::new();
    let (interaction, mut rx) = InteractionPlane::new();
    let mut ctx = ctx_with(vec![], PathBuf::from("."));
    ctx.session = Some(session);
    ctx.interaction = interaction.for_session(session);
    let ask1 = Arc::clone(&ask);
    let h = tokio::spawn(async move {
        ask1.execute(
            &ctx,
            json!({
                "questions": [
                    { "question": "Pick a color", "header": "Color", "options": [
                        { "label": "red", "description": "Warm" },
                        { "label": "green", "description": "Calm" }
                    ]},
                    { "question": "Branch name?", "header": "Branch", "options": [] }
                ]
            }),
        )
        .await
    });
    let req = rx.recv().await.unwrap();
    assert_eq!(req.session, Some(session));
    assert_eq!(req.questions.len(), 2);
    assert_eq!(
        req.kind,
        QuestionKind::Select {
            options: vec!["red".into(), "green".into()],
            allow_custom: true
        }
    );
    req.reply
        .send_many(vec![
            QuestionAnswer::Selected(1),
            QuestionAnswer::FreeText("codex/todo".into()),
        ])
        .unwrap();
    let out = h.await.unwrap().unwrap();
    assert_eq!(out["title"], "Asked 2 questions");
    assert_eq!(out["metadata"]["answers"][0]["answer"], json!(["green"]));
    assert_eq!(
        out["metadata"]["answers"][1]["answer"],
        json!(["codex/todo"])
    );

    // 2. Multi-select + allow_custom=false, legacy `custom` alias, cancel, validation, unavailable plane.
    let (interaction, mut rx) = InteractionPlane::new();
    let mut ctx = ctx_with(vec![], PathBuf::from("."));
    ctx.session = Some(session);
    ctx.interaction = interaction.for_session(session);
    let ask2 = Arc::clone(&ask);
    let ctx2 = ctx.clone();
    let h = tokio::spawn(async move {
        ask2.execute(
            &ctx2,
            json!({
                "questions": [{
                    "question": "Pick colors", "header": "Color", "multiple": true, "allow_custom": false,
                    "options": [
                        { "label": "red", "description": "Warm" },
                        { "label": "green", "description": "Calm" },
                        { "label": "blue", "description": "Cool" }
                    ]
                }]
            }),
        )
        .await
    });
    let req = rx.recv().await.unwrap();
    assert!(req.info.multiple);
    req.reply
        .send(QuestionAnswer::SelectedMany(vec![0, 2]))
        .unwrap();
    assert_eq!(
        h.await.unwrap().unwrap()["metadata"]["answers"][0]["answer"],
        json!(["red", "blue"])
    );

    // Legacy `custom: false` in the question input.
    let ctx3 = ctx.clone();
    let ask_legacy = Arc::clone(&ask);
    let h = tokio::spawn(async move {
        ask_legacy
            .execute(
                &ctx3,
                json!({ "questions": [{ "question": "Pick", "header": "h", "custom": false, "options": [{ "label": "a", "description": "d" }] }] }),
            )
            .await
    });
    let req = rx.recv().await.unwrap();
    assert_eq!(
        req.kind,
        QuestionKind::Select {
            options: vec!["a".into()],
            allow_custom: false
        }
    );
    req.reply.send(QuestionAnswer::Selected(0)).unwrap();
    assert_eq!(h.await.unwrap().unwrap()["title"], "Asked 1 question");

    // Cancel returns ToolError::Cancelled.
    let ask3 = Arc::clone(&ask);
    let ctx4 = ctx.clone();
    let h = tokio::spawn(async move {
        ask3.execute(
            &ctx4,
            json!({ "questions": [{ "question": "Ship?", "header": "c", "options": [{ "label": "yes", "description": "go" }] }] }),
        )
        .await
    });
    rx.recv()
        .await
        .unwrap()
        .reply
        .send(QuestionAnswer::Cancelled)
        .unwrap();
    assert!(matches!(h.await.unwrap(), Err(ToolError::Cancelled)));

    // Missing description rejected; dropped receiver surfaces unavailable.
    assert!(matches!(
        ask.execute(&ctx, json!({ "questions": [{ "question": "q", "header": "h", "options": [{ "label": "red" }] }] })).await,
        Err(ToolError::Input(m)) if m.contains("description")
    ));
    drop(rx);
    assert!(matches!(
        ask.execute(&ctx, json!({ "questions": [{ "question": "q", "header": "h", "options": [{ "label": "y", "description": "d" }] }] })).await,
        Err(ToolError::Other(m)) if m.contains("unavailable")
    ));
}
