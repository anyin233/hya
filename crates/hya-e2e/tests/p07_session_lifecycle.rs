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

/// T1.11 — the production backend titles a root session in the background
/// after its first prompt (fixed `title` agent), once: the title call never
/// consumes the turn's scripted steps, and a second prompt does not retitle.
#[tokio::test]
async fn t1_11_first_prompt_titles_the_session_once() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            text_step("TITLE_TURN_ONE"),
            text_step("TITLE_TURN_TWO"),
        ])
        .build()
        .await
        .expect("e2e env");
    env.fake
        .set_title_reply("Widget audit")
        .expect("title reply");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "audit the widget module")
        .await
        .expect("first prompt");
    let url = format!("{}/v1/sessions/{session}", env.backend.url);
    let mut title = serde_json::Value::Null;
    for _ in 0..200 {
        let info: serde_json::Value = reqwest::get(&url)
            .await
            .expect("get session")
            .json()
            .await
            .expect("session json");
        title = info["title"].clone();
        if !title.is_null() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(
        title,
        serde_json::json!("Widget audit"),
        "{}",
        env.diagnostics()
    );

    let _ = env
        .prompt(session, "now the gadget module")
        .await
        .expect("second prompt");
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(env.fake.title_requests().expect("title requests").len(), 1);
    assert_eq!(
        env.fake.remaining_scripts().expect("remaining"),
        0,
        "both turns consumed their own steps"
    );
    let titled = env
        .events(session, None)
        .await
        .expect("events")
        .into_iter()
        .filter(|envelope| matches!(envelope.event, Event::SessionTitled { .. }))
        .count();
    assert_eq!(titled, 1, "{}", env.diagnostics());
}
