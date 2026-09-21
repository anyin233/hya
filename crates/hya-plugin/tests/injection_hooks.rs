//! Integration tests for `hya-plugin`: the P5 injection-point hooks
//! (`compaction.before`/`compaction.after`, `session.start`/`session.end`,
//! `agent.spawn`) on the wire and through the `PluginHost` dispatcher.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use hya_core::hooks::{
    AgentSpawnInput, ChatParamsInput, CompactionAfterInput, CompactionBeforeInput,
    CompactionDecision, CompactionTrigger, HookDispatcher, SessionLifecycleInput,
};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::manifest::Manifest;
use hya_plugin::messages::{
    AgentSpawnParams, CompactionAfterParams, CompactionBeforeOutcomeWire, CompactionBeforeParams,
    CompactionTriggerWire, HookName, HookPosture, HostInfo, PluginKindWire, SessionLifecycleParams,
};
use hya_proto::{MessageId, ModelRef, SessionId};
use serde_json::json;

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

/// Task 5.1: the five new hook names serialize to exactly the documented wire
/// spellings, parse back, and default to the Open posture (they are
/// enrichment/observation points, not guards).
#[test]
fn new_hook_names_round_trip_and_default_open() {
    for (name, wire) in [
        (HookName::CompactionBefore, "compaction.before"),
        (HookName::CompactionAfter, "compaction.after"),
        (HookName::SessionStart, "session.start"),
        (HookName::SessionEnd, "session.end"),
        (HookName::AgentSpawn, "agent.spawn"),
    ] {
        assert_eq!(
            serde_json::to_string(&name).unwrap(),
            format!("\"{wire}\""),
            "{wire} must serialize to its documented snake_case spelling"
        );
        assert_eq!(HookName::from_wire(wire), Some(name));
        assert_eq!(name.method(), format!("hook/{wire}"));
        assert_eq!(
            name.default_posture(),
            HookPosture::Open,
            "{wire} is an observation/enrichment point and must default Open"
        );
    }
    // Protocol stays at version 1: unknown names are not names at all.
    assert_eq!(HookName::from_wire("compaction.beforex"), None);
}

/// Task 5.1: an old host reading a new plugin's `plugin.toml` must drop the
/// unknown hook declarations with a warning, not reject the manifest.
#[test]
fn manifest_resolved_hooks_drop_unknown_names_but_keep_new_known_ones() {
    let manifest = Manifest::parse(
        "id = \"future\"\ncommand = [\"x\"]\nhooks = [\n\
         \x20 { name = \"compaction.before\" },\n\
         \x20 { name = \"session.end\" },\n\
         \x20 { name = \"time.travel\" },\n\
         ]\n",
    )
    .unwrap();
    let resolved: BTreeSet<HookName> = manifest
        .resolved_hooks()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        resolved,
        BTreeSet::from([HookName::CompactionBefore, HookName::SessionEnd]),
        "known new names resolve; unknown names are dropped"
    );
}

/// Task 5.2: a plugin registering `compaction.before` receives the call and a
/// `Replace` outcome propagates verbatim to the engine-facing result.
#[tokio::test]
async fn compaction_before_reaches_plugin_and_replace_payload_propagates() {
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
            "plugin": {"id": "compact", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "compaction.before"}, {"name": "compaction.after"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/compaction.before":
        assert msg["params"]["trigger"] == "overflow", msg["params"]
        assert msg["params"]["messagesTokenEstimate"] > 0, msg["params"]
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "outcome": "replace", "instructions": "SUMMARIZE AS LIMERICKS"}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = PluginHost::connect_all(vec![spec("compact", script)], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let session = SessionId::new();
    let outcome = host
        .compaction_before(CompactionBeforeInput {
            session,
            trigger: CompactionTrigger::Overflow,
            messages_token_estimate: 4_200,
        })
        .await;
    match outcome {
        CompactionDecision::Replace { instructions } => {
            assert_eq!(instructions, "SUMMARIZE AS LIMERICKS");
        }
        other => panic!("expected Replace, got {other:?}"),
    }
}

/// Task 5.2: the chain folds in load order — a `Proceed` from the first plugin
/// keeps consulting the next, whose decisive outcome wins.
#[tokio::test]
async fn compaction_before_chain_folds_across_two_plugins() {
    let observer = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "observer", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "compaction.before"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/compaction.before":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "proceed"}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let decider = r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "decider", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "compaction.before"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/compaction.before":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {
            "outcome": "skip", "reason": "not worth folding yet"}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = PluginHost::connect_all(
        vec![spec("observer", observer), spec("decider", decider)],
        host_info(),
    )
    .await;
    assert_eq!(host.len(), 2, "both fixtures must connect");

    let outcome = host
        .compaction_before(CompactionBeforeInput {
            session: SessionId::new(),
            trigger: CompactionTrigger::Proactive,
            messages_token_estimate: 10,
        })
        .await;
    match outcome {
        CompactionDecision::Skip { reason } => {
            assert_eq!(reason, "not worth folding yet");
        }
        other => panic!("expected the decisive later plugin to win, got {other:?}"),
    }
}

/// Task 5.2 fail-open: a `compaction.before` hook error (crashed plugin
/// returning a JSON-RPC error) must fall back to Proceed — compaction is never
/// blocked by a broken hook.
#[tokio::test]
async fn compaction_before_hook_error_fails_open_to_proceed() {
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
            "plugin": {"id": "poison", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "compaction.before"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/compaction.before":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                          "error": {"code": -32000, "message": "poisoned"}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = PluginHost::connect_all(vec![spec("poison", script)], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let outcome = host
        .compaction_before(CompactionBeforeInput {
            session: SessionId::new(),
            trigger: CompactionTrigger::Overflow,
            messages_token_estimate: 10,
        })
        .await;
    assert_eq!(
        outcome,
        CompactionDecision::Proceed,
        "a failing compaction hook must never block built-in compaction"
    );
}

/// Task 5.2: notification hooks fan out to every registering plugin; params
/// round-trip through the wire shapes.
#[tokio::test]
async fn notification_hooks_fan_out_to_plugins() {
    let script = r#"
import json, os, sys
log = os.environ["HYA_SPAWN_LOG"]
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "watch", "version": "0.1.0", "kind": "rust"},
            "hooks": [
                {"name": "session.start"}, {"name": "session.end"},
                {"name": "agent.spawn"}, {"name": "compaction.after"},
            ],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/compaction.after":
        assert msg["params"]["summaryTokens"] == 128, msg["params"]
    elif msg.get("method") == "hook/agent.spawn":
        assert msg["params"]["parent"] != msg["params"]["child"], msg["params"]
    with open(log, "a") as handle:
        handle.write(msg.get("method", "?") + "\n")
    if "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let log = std::env::temp_dir().join(format!(
        "hya_injection_hooks_{}_{}.log",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut env = BTreeMap::new();
    env.insert(
        "HYA_SPAWN_LOG".to_string(),
        log.to_string_lossy().into_owned(),
    );
    let mut plugin = spec("watch", script);
    plugin.env = env;
    let host = PluginHost::connect_all(vec![plugin], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let session = SessionId::new();
    let child = SessionId::new();
    host.session_start(SessionLifecycleInput { session }).await;
    host.session_end(SessionLifecycleInput { session }).await;
    host.agent_spawn(AgentSpawnInput {
        parent: session,
        child,
    })
    .await;
    host.compaction_after(CompactionAfterInput {
        session,
        summary_tokens: 128,
    })
    .await;

    let mut delivered = BTreeSet::new();
    for _ in 0..50 {
        if let Ok(body) = std::fs::read_to_string(&log) {
            for line in body.lines() {
                delivered.insert(line.to_string());
            }
        }
        if delivered.len() >= 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = std::fs::remove_file(&log);
    for method in [
        "hook/session.start",
        "hook/session.end",
        "hook/agent.spawn",
        "hook/compaction.after",
    ] {
        assert!(delivered.contains(method), "{method} must reach the plugin");
    }
}

/// Wire shapes for the new hook params round-trip with the documented keys.
#[test]
fn new_hook_params_round_trip() {
    let session = SessionId::new();
    let before = CompactionBeforeParams {
        session,
        trigger: CompactionTriggerWire::Overflow,
        messages_token_estimate: 4_200,
    };
    let value = serde_json::to_value(&before).unwrap();
    assert_eq!(value["trigger"], json!("overflow"));
    assert_eq!(value["messagesTokenEstimate"], json!(4200));
    assert_eq!(
        serde_json::from_value::<CompactionBeforeParams>(value).unwrap(),
        before
    );

    let outcome = CompactionBeforeOutcomeWire::Replace {
        instructions: "do it differently".to_string(),
    };
    let value = serde_json::to_value(&outcome).unwrap();
    assert_eq!(value["outcome"], json!("replace"));
    assert_eq!(value["instructions"], json!("do it differently"));

    let after = CompactionAfterParams {
        session,
        summary_tokens: 128,
    };
    let value = serde_json::to_value(&after).unwrap();
    assert_eq!(value["summaryTokens"], json!(128));

    let lifecycle = SessionLifecycleParams { session };
    let value = serde_json::to_value(&lifecycle).unwrap();
    assert_eq!(value["session"], serde_json::to_value(session).unwrap());

    let spawn = AgentSpawnParams {
        parent: session,
        child: SessionId::new(),
    };
    let value = serde_json::to_value(&spawn).unwrap();
    assert_eq!(value["parent"], serde_json::to_value(spawn.parent).unwrap());
    assert_eq!(value["child"], serde_json::to_value(spawn.child).unwrap());
}

/// Task 5.3 fail-open pin: a `chat.params` hook that errors (here: a JSON-RPC
/// error reply) must not poison the model call — the original request flows
/// through unchanged, and the failure is only a logged warning.
#[tokio::test]
async fn chat_params_hook_error_fails_open_to_original_params() {
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
            "plugin": {"id": "poison-params", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "chat.params"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/chat.params":
        msg["params"]["request"]["model"] = "evil/model"
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"],
                          "error": {"code": -32000, "message": "no params for you"}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let host = PluginHost::connect_all(vec![spec("poison-params", script)], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let original = wire_request("fake/model");
    let outcome = host
        .chat_params(ChatParamsInput {
            session: SessionId::new(),
            message: MessageId::new(),
            request: original.clone(),
        })
        .await;
    match outcome {
        hya_core::hooks::ChatParamsOutcome::Continue { request } => {
            assert_eq!(
                request.model.as_str(),
                "fake/model",
                "a failed chat.params hook must leave the original request untouched"
            );
            assert_eq!(request.system, original.system);
            assert_eq!(request.temperature, original.temperature);
            assert_eq!(request.max_output_tokens, original.max_output_tokens);
        }
    }
}

/// A timeout on `chat.params` fails open the same way: the model call proceeds
/// with the original request.
#[tokio::test]
async fn chat_params_hook_timeout_fails_open_to_original_params() {
    let script = r#"
import json, sys, time
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "slow-params", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "chat.params"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif msg.get("method") == "hook/chat.params":
        time.sleep(5)
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue",
            "request": msg["params"]["request"]}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    let mut plugin = spec("slow-params", script);
    plugin.timeout_ms = Some(200);
    let host = PluginHost::connect_all(vec![plugin], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let original = wire_request("fake/model");
    let outcome = host
        .chat_params(ChatParamsInput {
            session: SessionId::new(),
            message: MessageId::new(),
            request: original.clone(),
        })
        .await;
    match outcome {
        hya_core::hooks::ChatParamsOutcome::Continue { request } => {
            assert_eq!(
                request.model.as_str(),
                "fake/model",
                "a timed-out chat.params hook must not block the model call"
            );
            assert_eq!(request.system, original.system);
            assert_eq!(request.temperature, original.temperature);
            assert_eq!(request.max_output_tokens, original.max_output_tokens);
        }
    }
}

fn wire_request(model: &str) -> hya_provider::CompletionRequest {
    hya_provider::CompletionRequest {
        model: ModelRef::new(model),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: BTreeMap::new(),
    }
}
