#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Phase 5: a plugin that crashes mid-call is marked `Dead`, the next call
//! respawns it, and exceeding the restart budget (>3 in the window) moves it to
//! `Disabled`.

use std::collections::BTreeMap;

use hya_core::hooks::{HookDispatcher, ToolExecuteBeforeInput};
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};
use hya_plugin::{PluginHost, PluginStatus};
use hya_proto::{MessageId, SessionId, ToolCallId};
use serde_json::json;

fn crashing_fixture() -> Vec<String> {
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
            "plugin": {"id": "crasher", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "tool.execute.before", "posture": "open"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "hook/tool.execute.before":
        sys.exit(1)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    vec!["python3".to_string(), "-c".to_string(), script.to_string()]
}

async fn poke(host: &PluginHost) {
    let _ = host
        .tool_execute_before(ToolExecuteBeforeInput {
            session: SessionId::new(),
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "shell".to_string(),
            input: json!({"command": "ls"}),
        })
        .await;
}

#[tokio::test]
async fn crash_marks_dead_then_respawns_then_disables() {
    let spec = PluginSpec {
        id: "crasher".to_string(),
        kind: PluginKindWire::Rust,
        command: crashing_fixture(),
        timeout_ms: Some(2000),
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
    assert_eq!(
        host.plugin_status("crasher").await,
        Some(PluginStatus::Alive),
        "fresh plugin is alive"
    );

    poke(&host).await;
    assert_eq!(
        host.plugin_status("crasher").await,
        Some(PluginStatus::Dead),
        "a crash mid-call marks the plugin dead"
    );

    poke(&host).await;
    assert_eq!(
        host.plugin_status("crasher").await,
        Some(PluginStatus::Dead),
        "the next call respawns; the respawn re-crashes so it is dead, not yet disabled"
    );

    let mut disabled = false;
    for _ in 0..10 {
        poke(&host).await;
        if host.plugin_status("crasher").await == Some(PluginStatus::Disabled) {
            disabled = true;
            break;
        }
    }
    assert!(
        disabled,
        "exceeding the restart budget must disable the plugin"
    );
}
