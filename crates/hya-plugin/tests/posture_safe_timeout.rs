#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Phase 5: a `safe`-posture `tool.execute.before` guard that misses its
//! deadline must fail safe — the chain vetoes so the engine emits a `ToolError`
//! rather than running the tool with an unchecked guard.

use std::collections::BTreeMap;

use hya_core::hooks::{HookDispatcher, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};
use hya_proto::{MessageId, SessionId, ToolCallId};
use serde_json::json;

fn slow_guard_fixture() -> Vec<String> {
    let script = r#"
import json, sys, time
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "slow-guard", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "tool.execute.before", "posture": "safe"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "hook/tool.execute.before":
        time.sleep(2)
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue", "input": msg["params"]["input"]}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    vec!["python3".to_string(), "-c".to_string(), script.to_string()]
}

#[tokio::test]
async fn tool_before_safe_timeout_vetoes() {
    let spec = PluginSpec {
        id: "slow-guard".to_string(),
        kind: PluginKindWire::Rust,
        command: slow_guard_fixture(),
        timeout_ms: Some(200),
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
        .tool_execute_before(ToolExecuteBeforeInput {
            session: SessionId::new(),
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "shell".to_string(),
            input: json!({"command": "rm -rf /"}),
        })
        .await;

    match outcome {
        ToolExecuteBeforeOutcome::Veto { reason } => {
            assert!(
                reason.contains("guard failed safe"),
                "veto reason should explain the safe failure, got: {reason}"
            );
        }
        ToolExecuteBeforeOutcome::Continue { .. } => {
            panic!("safe guard timeout must veto, not continue");
        }
    }
}
