//! T1.11 — session list + resume (second prompt on same session).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step};
use hya_proto::Event;

#[tokio::test]
async fn t1_11_session_list_and_resume_prompt() {
    // Compat /session hides empty_unnamed sessions until they have activity.
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            text_step("SESSION_FIRST"),
            text_step("SESSION_B_TURN"),
            text_step("SESSION_RESUMED"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session_a = env.create_session().await.expect("session a");
    let session_b = env.create_session().await.expect("session b");
    assert_ne!(session_a.to_string(), session_b.to_string());

    let _ = env
        .prompt(session_a, "first turn")
        .await
        .expect("first prompt");
    let _ = env
        .prompt(session_b, "second session")
        .await
        .expect("session b prompt");

    let listed = env.list_sessions_compat().await.expect("list sessions");
    let listed_text = listed.to_string();
    assert!(
        listed_text.contains(&session_a.to_string()),
        "session A must appear in list; listed={listed}; {}",
        env.diagnostics()
    );
    assert!(
        listed_text.contains(&session_b.to_string()),
        "session B must appear in list after activity; listed={listed}; {}",
        env.diagnostics()
    );

    let _ = env
        .prompt(session_a, "resume turn")
        .await
        .expect("resume prompt");

    let events = env.events(session_a, None).await.expect("events");
    let mut text = String::new();
    for env_evt in events {
        match env_evt.event {
            Event::TextDelta { delta, .. } => text.push_str(&delta),
            Event::TextReplace { text: t, .. } => text.push_str(&t),
            _ => {}
        }
    }
    assert!(
        text.contains("SESSION_FIRST"),
        "first turn text missing; text={text:?}; {}",
        env.diagnostics()
    );
    assert!(
        text.contains("SESSION_RESUMED"),
        "resume turn text missing; text={text:?}; {}",
        env.diagnostics()
    );
    assert!(
        env.fake.requests().unwrap().len() >= 3,
        "three prompts expected; {}",
        env.diagnostics()
    );
}
