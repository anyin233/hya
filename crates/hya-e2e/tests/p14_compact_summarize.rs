//! T1.14 — compact / summarize injects system summary into session context.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step};
use hya_proto::Event;

#[tokio::test]
async fn t1_14_compact_injects_summary_and_allows_follow_up_turn() {
    // FakeLlm queue: two normal turns, then ModelSummarizer completion, then post-compact turn.
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            text_step("COMPACT_TURN_ONE"),
            text_step("COMPACT_TURN_TWO"),
            text_step("E2E_COMPACT_SUMMARY_MARKER"),
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

    let context = env.session_context(&session).await.expect("context after compact");
    let blob = context.to_string();
    assert!(
        blob.contains("E2E_COMPACT_SUMMARY_MARKER")
            || blob.contains("Summary of earlier conversation"),
        "compact must inject a system summary into context; context={context}; {}",
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

    // Follow-up model request should still see prior transcript and/or summary.
    let requests = env.fake.requests().expect("requests");
    assert!(
        requests.len() >= 4,
        "two turns + summarize + follow-up expected; n={}; {}",
        requests.len(),
        env.diagnostics()
    );
}

#[tokio::test]
async fn t1_14_legacy_summarize_persists_summary_metadata_path() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            text_step("SUM_TURN_A"),
            text_step("SUM_TURN_B"),
            text_step("E2E_LEGACY_SUMMARY_MARKER"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env.prompt(session, "legacy summarize A").await.expect("a");
    let _ = env.prompt(session, "legacy summarize B").await.expect("b");

    let result = env
        .summarize_session_legacy(&session)
        .await
        .expect("legacy summarize");
    // Endpoint returns JSON `true` on success.
    assert!(
        result.as_bool() == Some(true) || result == serde_json::json!(true),
        "summarize should return true; got={result}; {}",
        env.diagnostics()
    );

    let context = env.session_context(&session).await.expect("context");
    let blob = context.to_string();
    assert!(
        blob.contains("E2E_LEGACY_SUMMARY_MARKER")
            || blob.contains("Summary of earlier conversation"),
        "legacy summarize must inject summary text; context={context}; {}",
        env.diagnostics()
    );
}
