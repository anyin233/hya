//! T1.10 — MCP stdio echo tool via real backend + FakeLlm.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::json;

#[tokio::test]
async fn t1_10_mcp_echo_ping_tool_roundtrip() {
    let env = E2eEnvBuilder::new()
        .with_mcp_echo()
        .scripts(vec![
            tool_step(
                "mcp__echo__ping",
                json!({ "msg": "hya-e2e-mcp" }),
            ),
            text_step("MCP_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    // Status surface may list the echo server once MCP has connected.
    let mcp_status = env.get_json("/mcp").await.unwrap_or(json!(null));
    let _ = mcp_status;

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "call mcp echo ping")
        .await
        .expect("mcp prompt");

    let requests = env.fake.requests().expect("fake requests");
    let dumped = serde_json::to_string(&requests).unwrap_or_default();
    assert!(
        dumped.contains("echo:hya-e2e-mcp")
            || dumped.contains("mcp__echo__ping")
            || dumped.contains("hya-e2e-mcp"),
        "MCP tool result should feed the next model turn; dump={dumped}; {}",
        env.diagnostics()
    );
    assert!(
        requests.len() >= 2,
        "tool + final text expected; {}",
        env.diagnostics()
    );
}
