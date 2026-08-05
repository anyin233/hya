//! T1.10 — MCP stdio echo tool via real backend + FakeLlm.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::json;

#[tokio::test]
async fn t1_10_mcp_echo_ping_tool_roundtrip() {
    let env = E2eEnvBuilder::new()
        .with_mcp_echo()
        .scripts(vec![
            tool_step("mcp__echo__ping", json!({ "msg": "hya-e2e-mcp" })),
            text_step("MCP_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    let status = env
        .wait_mcp_connected("echo", Duration::from_secs(20))
        .await
        .expect("mcp echo connected");
    assert_eq!(
        status
            .get("echo")
            .and_then(|s| s.get("status"))
            .and_then(|s| s.as_str()),
        Some("connected"),
        "mcp status={status}; {}",
        env.diagnostics()
    );

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "call mcp echo ping")
        .await
        .expect("mcp prompt");

    let requests = env.fake.requests().expect("fake requests");
    assert!(
        requests.len() >= 2,
        "tool turn + follow-up model turn required; {}",
        env.diagnostics()
    );
    // Only the MCP server success text (`echo:{msg}` from mcp_echo_script) proves
    // the tool actually ran. Tool-call args on request[0] always include the name
    // and msg even when MCP fails.
    let follow_up = fake_requests_from(&requests, 1);
    assert!(
        follow_up.contains("echo:hya-e2e-mcp"),
        "follow-up FakeLlm request must include MCP tool result echo:hya-e2e-mcp; follow_up={follow_up}; {}",
        env.diagnostics()
    );
}
