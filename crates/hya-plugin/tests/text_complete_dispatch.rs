#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use hya_core::hooks::{HookDispatcher, TextCompleteInput, TextCompleteOutcome};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};
use hya_proto::{MessageId, PartId, SessionId};

fn text_complete_fixture() -> Vec<String> {
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
            "plugin": {"id": "text-complete", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "experimental.text.complete", "posture": "open"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "hook/experimental.text.complete":
        params = msg["params"]
        text = f'{params["text"]} [{params["message"]}:{params["part"]}]'
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue", "text": text}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    vec!["python3".to_string(), "-c".to_string(), script.to_string()]
}

#[tokio::test]
async fn text_complete_dispatches_through_plugin_host() {
    // Given: a connected plugin advertises experimental.text.complete.
    let spec = PluginSpec {
        id: "text-complete".to_string(),
        kind: PluginKindWire::Rust,
        command: text_complete_fixture(),
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

    // When: the engine-facing hook is dispatched through the plugin host.
    let outcome = host
        .text_complete(TextCompleteInput {
            session: SessionId::new(),
            message: MessageId::new(),
            part: PartId::new(),
            text: "draft".to_string(),
        })
        .await;

    // Then: the plugin's rewritten text is returned to the engine.
    match outcome {
        TextCompleteOutcome::Continue { text } => {
            assert!(text.starts_with("draft ["));
        }
    }
}
