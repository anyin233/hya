//! T1.12 — multi-turn conversation exposed via Compat `/api/session/{id}/context`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step};

#[tokio::test]
async fn t1_12_session_context_lists_user_and_assistant_turns() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            text_step("CONTEXT_TURN_ONE"),
            text_step("CONTEXT_TURN_TWO"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "first user turn about ALPHA_TOPIC")
        .await
        .expect("prompt 1");
    let _ = env
        .prompt(session, "second user turn about BETA_TOPIC")
        .await
        .expect("prompt 2");

    let context = env.session_context(&session).await.expect("context");
    let data = context
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        data.len() >= 2,
        "context must include multiple messages; context={context}; {}",
        env.diagnostics()
    );

    let blob = context.to_string();
    assert!(
        blob.contains("ALPHA_TOPIC") || blob.contains("first user"),
        "user prompt text must appear in context projection; context={context}; {}",
        env.diagnostics()
    );
    assert!(
        blob.contains("CONTEXT_TURN_ONE"),
        "first assistant text must appear in context; context={context}; {}",
        env.diagnostics()
    );
    assert!(
        blob.contains("CONTEXT_TURN_TWO"),
        "second assistant text must appear in context; context={context}; {}",
        env.diagnostics()
    );
    assert!(
        blob.contains("user") && blob.contains("assistant"),
        "context should include user and assistant roles; context={context}; {}",
        env.diagnostics()
    );
}
