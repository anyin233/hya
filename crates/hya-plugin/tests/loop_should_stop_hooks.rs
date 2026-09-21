//! Integration tests for `hya-plugin`: the `loop.should_stop` wire hook
//! (dev_plan 6.5) — round-trip + Open posture, dispatch to the registered
//! plugin chain, and fail-open tolerance for errors and malformed replies.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use hya_core::hooks::HookDispatcher;
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HookName, HookPosture, HostInfo, PluginKindWire};

fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: "0.0.0".to_string(),
    }
}

fn spec(id: &str, script: &str) -> PluginSpec {
    PluginSpec {
        id: id.to_string(),
        kind: PluginKindWire::Rust,
        command: vec!["python3".to_string(), "-c".to_string(), script.to_string()],
        timeout_ms: Some(4000),
        env: BTreeMap::new(),
        posture_overrides: BTreeMap::new(),
    }
}

/// `loop.should_stop` uses its documented wire spelling, round-trips through
/// serde, and defaults to the Open posture: it is a post-turn consult, not a
/// guard, so a broken hook must never decide the loop by itself.
#[test]
fn loop_should_stop_name_round_trips_and_defaults_open() {
    assert_eq!(
        serde_json::to_string(&HookName::LoopShouldStop).unwrap(),
        "\"loop.should_stop\""
    );
    assert_eq!(
        HookName::from_wire("loop.should_stop"),
        Some(HookName::LoopShouldStop)
    );
    assert_eq!(HookName::LoopShouldStop.method(), "hook/loop.should_stop");
    assert_eq!(
        HookName::LoopShouldStop.default_posture(),
        HookPosture::Open,
        "loop.should_stop is a consult, not a guard, and must default Open"
    );
}

/// dev_plan 6.5: a plugin registering `loop.should_stop` receives the target
/// and transcript, and a `{"stop": true, "reason": ...}` reply surfaces as
/// `Some(reason)` — the engine-side stop signal.
#[tokio::test]
async fn loop_should_stop_reaches_registered_plugin_and_surfaces_reason() {
    let script = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "gate", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "loop.should_stop"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/loop.should_stop":
        assert msg["params"]["target"] == "green tests", msg["params"]
        assert "worker output" in msg["params"]["transcript"], msg["params"]
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "stop": True, "reason": "worker reports done"}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = PluginHost::connect_all(vec![spec("gate", script)], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let stop = host
        .loop_should_stop("green tests", "[User] worker output")
        .await;
    assert_eq!(
        stop.as_deref(),
        Some("worker reports done"),
        "Some(reason) is the engine-side stop signal"
    );
}

/// A `{"stop": false}` reply keeps the loop going: `None`, not an error.
#[tokio::test]
async fn loop_should_stop_continue_reply_is_none() {
    let script = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "watcher", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "loop.should_stop"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/loop.should_stop":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "stop": False}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = PluginHost::connect_all(vec![spec("watcher", script)], host_info()).await;
    assert_eq!(
        host.loop_should_stop("target", "transcript").await,
        None,
        "stop=false must read as keep-going"
    );
}

/// Without a registered provider the hook answers `None` (default trait impl):
/// the gate falls through to the verifier untouched.
#[tokio::test]
async fn loop_should_stop_without_registration_defaults_to_none() {
    let host = PluginHost::connect_all(Vec::new(), host_info()).await;
    assert_eq!(host.loop_should_stop("target", "transcript").await, None);
}

/// Fail-open (Open posture): a registered provider whose reply is not a
/// parseable `{stop, reason}` object, or whose transport fails, must degrade
/// to `None` — a broken hook never forces or blocks a stop.
#[tokio::test]
async fn loop_should_stop_errors_and_malformed_replies_fail_open_to_none() {
    let malformed = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "sloppy", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "loop.should_stop"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/loop.should_stop":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                          "result": {"stop": "yes", "verdict": True}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = PluginHost::connect_all(vec![spec("sloppy", malformed)], host_info()).await;
    assert_eq!(
        host.loop_should_stop("target", "transcript").await,
        None,
        "a malformed reply must fail open to None, never to a stop"
    );
}
