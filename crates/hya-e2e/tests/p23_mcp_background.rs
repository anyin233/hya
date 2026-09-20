//! T23 — long MCP call auto-backgrounds and the completion steers a reclaim
//! prompt end to end: real backend + real stdio MCP child + FakeLlm.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::{Value, json};

#[tokio::test]
async fn t23_mcp_slow_call_backgrounds_then_steers_reclaim_turn() {
    let env = E2eEnvBuilder::new()
        .with_mcp_echo()
        // The fixture's slow tool sleeps 2s; the turn moves on after 300ms.
        .backend_env("HYA_MCP_BACKGROUND_AFTER_MS", "300")
        .scripts(vec![
            tool_step("mcp__echo__slow", json!({ "seconds": 2 })),
            text_step("MEANWHILE"),
            text_step("RECLAIMED"),
        ])
        .build()
        .await
        .expect("e2e env");

    let _status = env
        .wait_mcp_connected("echo", Duration::from_secs(20))
        .await
        .expect("mcp echo connected");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "call mcp echo slow")
        .await
        .expect("prompt");

    env.wait_session_idle(&session, Duration::from_secs(20))
        .await
        .expect("turn 1 idle after backgrounding");

    // Turn 1's follow-up request must see the backgrounded marker, not the
    // tool's real output (the call is still sleeping).
    let requests = env.fake.requests().expect("fake requests");
    let turn1_follow_up = fake_requests_from(&requests, 1);
    assert!(
        turn1_follow_up.contains("backgrounded") && turn1_follow_up.contains("mcpbg-"),
        "turn 1 follow-up must carry the backgrounded marker; got: {turn1_follow_up}; {}",
        env.diagnostics()
    );

    // The background watcher admits the reclaim prompt and the server's
    // driver runs a follow-up turn on the now-idle session; its model request
    // must contain the real tool result.
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    loop {
        let requests = env.fake.requests().expect("fake requests");
        let reclaimed = requests.len() >= 3 && fake_requests_from(&requests, 2).contains("slept:2");
        if reclaimed {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "reclaim turn never ran; requests={}; {}",
            requests.len(),
            env.diagnostics()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let requests = env.fake.requests().expect("fake requests");
    let reclaim_request = fake_requests_from(&requests, 2);
    assert!(
        reclaim_request.contains("[background job") && reclaim_request.contains("Reclaim"),
        "reclaim turn must be steered by the completion prompt; got: {reclaim_request}"
    );

    // Durable record: the session log carries the backgrounded early result
    // and the steered user prompt.
    let events = env.events(session, None).await.expect("session events");
    let saw_marker = events.iter().any(|envelope| match &envelope.event {
        hya_proto::Event::ToolResult { output, .. } => {
            output
                .get("metadata")
                .and_then(|m| m.get("backgrounded"))
                .and_then(Value::as_bool)
                == Some(true)
        }
        _ => false,
    });
    assert!(
        saw_marker,
        "durable backgrounded marker missing; events={events:?}"
    );
}
