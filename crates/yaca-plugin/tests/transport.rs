#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};
use yaca_plugin::PluginError;
use yaca_plugin::client::{DEFAULT_CALL_TIMEOUT, PluginClient};
use yaca_plugin::messages::{HookName, HostInfo, PluginKindWire};

const FIXTURE: &str = r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if req.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "fixture", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "tool.execute.before", "posture": "safe"}, {"name": "event"}],
            "tools": [{"name": "remember", "description": "", "inputSchema": {"type": "object"}}],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
"#;

#[tokio::test]
async fn demuxes_responses_by_id() {
    let (client_io, server_io) = duplex(4096);
    let (client_read, client_write) = split(client_io);
    let (server_read, mut server_write) = split(server_io);
    let client = PluginClient::new(client_read, client_write);

    let server = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let first: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let second: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let second_resp = json!({"jsonrpc":"2.0","id": second["id"], "result": {"second": true}});
        let first_resp = json!({"jsonrpc":"2.0","id": first["id"], "result": {"first": true}});
        server_write
            .write_all(format!("{second_resp}\n{first_resp}\n").as_bytes())
            .await
            .unwrap();
    });

    let first = client.call("first", json!({}), DEFAULT_CALL_TIMEOUT);
    let second = client.call("second", json!({}), DEFAULT_CALL_TIMEOUT);
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.unwrap(), json!({"first": true}));
    assert_eq!(second.unwrap(), json!({"second": true}));
    server.await.unwrap();
}

#[tokio::test]
async fn returns_timeout_errors() {
    let (client_io, _server_io) = duplex(4096);
    let (client_read, client_write) = split(client_io);
    let client = PluginClient::new(client_read, client_write);
    let result = client
        .call("slow", json!({}), Duration::from_millis(10))
        .await;
    assert!(matches!(result, Err(PluginError::Timeout { method }) if method == "slow"));
}

#[tokio::test]
async fn rpc_error_reply_maps_to_rpc_error() {
    let (client_io, server_io) = duplex(4096);
    let (client_read, client_write) = split(client_io);
    let (server_read, mut server_write) = split(server_io);
    let client = PluginClient::new(client_read, client_write);
    tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let req: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let resp =
            json!({"jsonrpc":"2.0","id": req["id"], "error": {"code": -32601, "message": "nope"}});
        server_write
            .write_all(format!("{resp}\n").as_bytes())
            .await
            .unwrap();
    });
    let result = client.call("x", json!({}), DEFAULT_CALL_TIMEOUT).await;
    assert!(matches!(result, Err(PluginError::Rpc { code: -32601, .. })));
}

#[tokio::test]
async fn handshake_with_fixture_reports_hooks_and_tools() {
    let command = vec!["python3".to_string(), "-c".to_string(), FIXTURE.to_string()];
    let (client, _guard) = PluginClient::spawn(&command, None).unwrap();
    let init = client
        .initialize(HostInfo {
            name: "yaca".to_string(),
            version: "0.0.0".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(init.protocol_version, 1);
    assert_eq!(init.plugin.id, "fixture");
    assert_eq!(init.plugin.kind, PluginKindWire::Rust);
    let hook_names: Vec<HookName> = init.hooks.iter().map(|h| h.name).collect();
    assert!(hook_names.contains(&HookName::ToolExecuteBefore));
    assert!(hook_names.contains(&HookName::Event));
    assert_eq!(init.tools.len(), 1);
    assert_eq!(init.tools[0].name, "remember");
}
