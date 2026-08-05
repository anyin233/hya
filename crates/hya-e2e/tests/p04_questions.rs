//! T1.8 — question tool + Compat question reply.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use hya_proto::Event;
use serde_json::json;

#[tokio::test]
async fn t1_8_question_tool_receives_user_answer() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step(
                "question",
                json!({
                    "questions": [{
                        "question": "Ship it?",
                        "header": "confirm",
                        "options": [
                            { "label": "yes", "description": "proceed" },
                            { "label": "no", "description": "abort" }
                        ]
                    }]
                }),
            ),
            text_step("QUESTION_ANSWERED"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt_with_question_reply(
            session,
            "ask the user",
            json!([["yes"]]),
            Duration::from_secs(30),
        )
        .await
        .expect("prompt+question");

    let events = env.events(session, None).await.expect("events");
    let mut saw_done = false;
    let mut text_bits = String::new();
    for env_evt in events {
        match env_evt.event {
            Event::TextDelta { delta, .. } => {
                if delta.contains("QUESTION_ANSWERED") {
                    saw_done = true;
                }
                text_bits.push_str(&delta);
            }
            Event::TextReplace { text, .. } => {
                if text.contains("QUESTION_ANSWERED") {
                    saw_done = true;
                }
                text_bits.push_str(&text);
            }
            _ => {}
        }
    }
    assert!(
        saw_done || text_bits.contains("QUESTION_ANSWERED"),
        "assistant should continue after question reply; text={text_bits:?}; {}",
        env.diagnostics()
    );
    assert!(
        env.fake.requests().unwrap().len() >= 2,
        "question turn + follow-up expected; {}",
        env.diagnostics()
    );
}
