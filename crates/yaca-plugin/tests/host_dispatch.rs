#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Phase 4: a `PluginHost` over a single fixture mutates `tool.execute.before`
//! input and delivers `event` notifications over the bounded channel.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use yaca_core::hooks::{HookDispatcher, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome};
use yaca_plugin::PluginHost;
use yaca_plugin::config::PluginSpec;
use yaca_plugin::messages::{HostInfo, PluginKindWire};
use yaca_proto::{Envelope, Event, EventSeq, MessageId, SessionId, ToolCallId};

fn fixture() -> Vec<String> {
    let script = r#"
import json, os, sys
log = os.environ.get("YACA_FIXTURE_LOG")
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "fix", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "tool.execute.before", "posture": "safe"}, {"name": "event"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "hook/tool.execute.before":
        inp = msg["params"]["input"]
        inp["mutated"] = True
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue", "input": inp}}), flush=True)
    elif method == "event":
        if log:
            with open(log, "a") as handle:
                handle.write("event\n")
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    vec!["python3".to_string(), "-c".to_string(), script.to_string()]
}

fn host_info() -> HostInfo {
    HostInfo {
        name: "yaca".to_string(),
        version: "0.0.0".to_string(),
    }
}

fn temp_log() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!(
            "yaca_host_dispatch_{}_{nanos}.log",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn before_hook_mutates_input_and_event_reaches_plugin() {
    let log = temp_log();
    let mut env = BTreeMap::new();
    env.insert("YACA_FIXTURE_LOG".to_string(), log.clone());
    let spec = PluginSpec {
        id: "fix".to_string(),
        kind: PluginKindWire::Rust,
        command: fixture(),
        timeout_ms: Some(3000),
        env,
        posture_overrides: BTreeMap::new(),
    };

    let host = PluginHost::connect_all(vec![spec], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let session = SessionId::new();
    let outcome = host
        .tool_execute_before(ToolExecuteBeforeInput {
            session,
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "shell".to_string(),
            input: json!({"command": "ls"}),
        })
        .await;
    match outcome {
        ToolExecuteBeforeOutcome::Continue { input } => {
            assert_eq!(input["mutated"], json!(true));
            assert_eq!(input["command"], json!("ls"));
        }
        ToolExecuteBeforeOutcome::Veto { reason } => panic!("unexpected veto: {reason}"),
    }

    let envelope = Envelope {
        seq: EventSeq(1),
        ts_millis: 0,
        event: Event::SessionTitled {
            session,
            title: "hello".to_string(),
        },
    };
    host.dispatch_event(&envelope);

    let mut delivered = false;
    for _ in 0..50 {
        if std::fs::read_to_string(&log)
            .map(|body| body.contains("event"))
            .unwrap_or(false)
        {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = std::fs::remove_file(&log);
    assert!(delivered, "event notification must reach the plugin");
}
