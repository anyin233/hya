//! T0.1 / T1.2 — backend boots; scripted prompt returns assistant text.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, ScriptStep};
use hya_proto::Event;

#[tokio::test]
async fn t0_1_and_t1_2_session_prompt_streams_fake_text() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![ScriptStep::Text("E2E_HELLO".into())])
        .build()
        .await
        .expect("e2e env");

    // T0.1: backend URL is live.
    let health = reqwest::get(format!("{}/global/health", env.backend.url))
        .await
        .expect("health request");
    assert!(
        health.status().is_success() || health.status().as_u16() == 404,
        "server should respond on {}",
        env.backend.url
    );

    let session = env.create_session().await.expect("create session");
    let prompt = env
        .prompt(session, "say hello")
        .await
        .expect("prompt completes");
    let _ = prompt.finish;

    let events = env.events(session, None).await.expect("events");
    let mut saw_text = false;
    let mut kinds = Vec::new();
    for env_evt in &events {
        kinds.push(
            format!("{:?}", env_evt.event)
                .chars()
                .take(80)
                .collect::<String>(),
        );
        if let Event::TextDelta { delta, .. } = &env_evt.event {
            if delta.contains("E2E_HELLO") {
                saw_text = true;
            }
        }
        if let Event::TextReplace { text, .. } = &env_evt.event {
            if text.contains("E2E_HELLO") {
                saw_text = true;
            }
        }
    }
    let fake_n = env.fake.requests().unwrap().len();
    assert!(
        saw_text,
        "assistant text E2E_HELLO must appear in events; fake_requests={fake_n}; event_kinds={kinds:?}"
    );
    assert!(env.fake_saw_request().unwrap(), "fake llm must be hit");
}
