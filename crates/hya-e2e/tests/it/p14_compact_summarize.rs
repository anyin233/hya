//! T1.14 — compact / summarize removes earlier context behind the kernel's
//! Rewrite marker, which repeats the removed user messages verbatim.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step};
use hya_proto::Event;
use serde_json::Value;

#[tokio::test]
async fn t1_14_compact_injects_summary_and_allows_follow_up_turn() {
    // FakeLlm queue: two normal turns, then the post-compact turn. The
    // compaction itself makes no model call.
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            text_step("COMPACT_TURN_ONE"),
            text_step("COMPACT_TURN_TWO"),
            text_step("AFTER_COMPACT_OK"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "remember detail ONE about widget-alpha")
        .await
        .expect("turn 1");
    let _ = env
        .prompt(session, "remember detail TWO about widget-beta")
        .await
        .expect("turn 2");

    env.compact_session(&session)
        .await
        .expect("compact session");

    let context = env
        .session_context(&session)
        .await
        .expect("context after compact");
    let blob = context.to_string();
    assert!(
        blob.contains("Earlier context was removed by the harness")
            && blob.contains("remember detail ONE about widget-alpha"),
        "compact must add the rewrite marker with the removed user message; context={context}; {}",
        env.diagnostics()
    );

    let _ = env
        .prompt(session, "continue after compaction")
        .await
        .expect("post-compact turn");

    let events = env.events(session, None).await.expect("events");
    let mut text = String::new();
    for env_evt in events {
        match env_evt.event {
            Event::TextDelta { delta, .. } => text.push_str(&delta),
            Event::TextReplace { text: t, .. } => text.push_str(&t),
            _ => {}
        }
    }
    assert!(
        text.contains("AFTER_COMPACT_OK"),
        "session must accept a follow-up turn after compact; text={text:?}; {}",
        env.diagnostics()
    );

    // The follow-up request carries the marker instead of turn 1.
    let requests = env.fake.requests().expect("requests");
    let last = requests.last().unwrap().to_string();
    assert!(last.contains("Earlier context was removed by the harness"));
    assert!(
        !last.contains("COMPACT_TURN_ONE"),
        "turn 1 left the request"
    );
}

#[tokio::test]
async fn t1_14_legacy_summarize_persists_summary_metadata_path() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![text_step("SUM_TURN_A"), text_step("SUM_TURN_B")])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env.prompt(session, "legacy summarize A").await.expect("a");
    let _ = env.prompt(session, "legacy summarize B").await.expect("b");

    let result = env
        .post_json(&format!("/v1/sessions/{session}/summarize"), &Value::Null)
        .await
        .expect("legacy summarize");
    // v1 returns the marker message id.
    assert!(
        result
            .get("summaryMessage")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        "summarize must return a summary message id; got={result}; {}",
        env.diagnostics()
    );

    let context = env.session_context(&session).await.expect("context");
    let blob = context.to_string();
    assert!(
        blob.contains("Earlier context was removed by the harness")
            && blob.contains("legacy summarize A"),
        "legacy summarize must add the rewrite marker; context={context}; {}",
        env.diagnostics()
    );
}
