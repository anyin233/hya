#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use hya_core::hooks::{CommandExecuteBeforeInput, CommandExecuteBeforeOutcome, HookDispatcher};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};
use hya_proto::SessionId;

fn command_fixture() -> Vec<String> {
    let script = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "command", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "command.execute.before", "posture": "open"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "hook/command.execute.before":
        params = msg["params"]
        text = f'{params["text"]} [{params["command"]}:{params["arguments"]}]'
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue", "text": text}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    vec!["python3".to_string(), "-c".to_string(), script.to_string()]
}

#[tokio::test]
async fn command_execute_before_dispatches_through_plugin_host() {
    let spec = PluginSpec {
        id: "command".to_string(),
        kind: PluginKindWire::Rust,
        command: command_fixture(),
        timeout_ms: Some(3000),
        env: BTreeMap::new(),
        posture_overrides: BTreeMap::new(),
    };
    let host = PluginHost::connect_all(
        vec![spec],
        HostInfo {
            name: "hya".to_string(),
            version: "0.0.0".to_string(),
        },
    )
    .await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let outcome = host
        .command_execute_before(CommandExecuteBeforeInput {
            session: SessionId::new(),
            command: "review".to_string(),
            arguments: "commit".to_string(),
            text: "Review diff".to_string(),
        })
        .await;

    match outcome {
        CommandExecuteBeforeOutcome::Continue { text } => {
            assert_eq!(text, "Review diff [review:commit]");
        }
    }
}
