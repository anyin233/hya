//! T1.10 — MCP stdio echo tool via real backend + FakeLlm.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::{Value, json};

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
    assert!(
        status
            .get("servers")
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["name"] == "echo" && row["state"] == "MCP_SERVER_STATE_CONNECTED"
                })
            }),
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

#[tokio::test]
async fn get_v1_mcp_lists_a_connected_servers_tools() {
    let env = E2eEnvBuilder::new()
        .with_mcp_echo()
        .scripts(vec![text_step("UNUSED")])
        .build()
        .await
        .expect("e2e env");
    env.wait_mcp_connected("echo", Duration::from_secs(20))
        .await
        .expect("mcp echo connected");

    let status = env.get_json("/v1/mcp").await.expect("mcp status");
    let echo = status["servers"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == "echo"))
        .unwrap_or_else(|| panic!("echo row missing: {status}"));
    assert_eq!(
        echo["tools"],
        json!(["mcp__echo__ping", "mcp__echo__slow"]),
        "a CONNECTED server reports its namespaced tools; status={status}"
    );

    env.post_json("/v1/mcp/echo/disconnect", &Value::Null)
        .await
        .expect("disconnect");
    let status = env.get_json("/v1/mcp").await.expect("disabled status");
    assert_eq!(
        status["servers"][0]["state"], "MCP_SERVER_STATE_DISCONNECTED",
        "{status}"
    );
    assert!(
        status["servers"][0]
            .get("tools")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty),
        "a disconnected server reports no tools; status={status}"
    );
}
