#![allow(clippy::unwrap_used, clippy::expect_used)]
//! [D5] The hook chain must fold in declared LOAD order, not `JoinSet`
//! handshake-completion order. The first-loaded plugin handshakes slowly; if the
//! host ordered by completion the tags would invert.

use std::collections::BTreeMap;

use hya_core::hooks::{HookDispatcher, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};
use hya_proto::{MessageId, SessionId, ToolCallId};
use serde_json::json;

fn tagging_fixture(tag: &str, delay_secs: &str) -> Vec<String> {
    let script = r#"
import json, sys, time
tag = sys.argv[1]
delay = float(sys.argv[2])
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        time.sleep(delay)
        result = {
            "protocol_version": 1,
            "plugin": {"id": tag, "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "tool.execute.before", "posture": "open"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "hook/tool.execute.before":
        inp = msg["params"]["input"]
        inp.setdefault("tags", []).append(tag)
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue", "input": inp}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    vec![
        "python3".to_string(),
        "-c".to_string(),
        script.to_string(),
        tag.to_string(),
        delay_secs.to_string(),
    ]
}

fn spec(id: &str, tag: &str, delay_secs: &str) -> PluginSpec {
    PluginSpec {
        id: id.to_string(),
        kind: PluginKindWire::Rust,
        command: tagging_fixture(tag, delay_secs),
        timeout_ms: Some(5000),
        env: BTreeMap::new(),
        posture_overrides: BTreeMap::new(),
    }
}

#[tokio::test]
async fn chain_folds_in_load_order_not_handshake_order() {
    let host = PluginHost::connect_all(
        vec![
            spec("first", "first", "0.5"),
            spec("second", "second", "0.0"),
        ],
        HostInfo {
            name: "hya".to_string(),
            version: "0.0.0".to_string(),
        },
    )
    .await;
    assert_eq!(host.len(), 2, "both fixtures must connect");
    assert_eq!(host.plugin_ids(), vec!["first", "second"]);

    let outcome = host
        .tool_execute_before(ToolExecuteBeforeInput {
            session: SessionId::new(),
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "shell".to_string(),
            input: json!({}),
        })
        .await;

    match outcome {
        ToolExecuteBeforeOutcome::Continue { input } => {
            assert_eq!(input["tags"], json!(["first", "second"]));
        }
        ToolExecuteBeforeOutcome::Veto { reason } => panic!("unexpected veto: {reason}"),
    }
}
