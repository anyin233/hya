//! Integration tests for `hya-plugin`: the `model.fallback` hook on the wire
//! and through the `PluginHost` dispatcher chain.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use hya_core::hooks::{
    HookDispatcher, ModelFailureClass, ModelFallbackInput, ModelFallbackOutcome,
};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{
    HookName, HookPosture, HostInfo, ModelFallbackOutcomeWire, ModelFallbackParams, PluginKindWire,
};
use hya_proto::{AgentName, MessageId, ModelRef, SessionId};
use serde_json::json;

fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: "0.0.0".to_string(),
    }
}

/// A python plugin registering `model.fallback` that answers with `reply`
/// (a Python expression over `p`, the params) or, when `reply` is `None`,
/// with a JSON-RPC error.
fn plugin(id: &str, reply: Option<&str>) -> PluginSpec {
    let answer = match reply {
        Some(expr) => format!(
            "print(json.dumps({{'jsonrpc': '2.0', 'id': msg['id'], 'result': {expr}}}), flush=True)"
        ),
        None => "print(json.dumps({'jsonrpc': '2.0', 'id': msg['id'], 'error': {'code': -32000, 'message': 'boom'}}), flush=True)".to_string(),
    };
    let script = format!(
        r#"
import json, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        result = {{
            "protocol_version": 1,
            "plugin": {{"id": "{id}", "version": "0.1.0", "kind": "rust"}},
            "hooks": [{{"name": "model.fallback"}}],
            "tools": [],
        }}
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": result}}), flush=True)
    elif msg.get("method") == "hook/model.fallback":
        p = msg["params"]
        {answer}
    elif "id" in msg:
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": {{}}}}), flush=True)
"#
    );
    PluginSpec {
        id: id.to_string(),
        kind: PluginKindWire::Rust,
        command: vec!["python3".to_string(), "-c".to_string(), script],
        timeout_ms: Some(4000),
        env: BTreeMap::new(),
        posture_overrides: BTreeMap::new(),
        plugin_dir: None,
    }
}

fn input(root: SessionId) -> ModelFallbackInput {
    ModelFallbackInput {
        session: SessionId::new(),
        root_session: root,
        agent: Some(AgentName::new("explore")),
        message: MessageId::new(),
        model: ModelRef::new("anthropic/claude-opus-5-5"),
        error_class: ModelFailureClass::Retryable,
        error_message: "http status 529: overloaded".to_string(),
        attempt: 2,
        tried: vec![
            ModelRef::new("anthropic/claude-opus-5-5"),
            ModelRef::new("anthropic/claude-sonnet-5"),
        ],
    }
}

#[test]
fn model_fallback_name_round_trips_and_defaults_open() {
    let name = HookName::ModelFallback;
    assert_eq!(serde_json::to_string(&name).unwrap(), "\"model.fallback\"");
    assert_eq!(HookName::from_wire("model.fallback"), Some(name));
    assert_eq!(name.method(), "hook/model.fallback");
    assert_eq!(name.default_posture(), HookPosture::Open);
}

#[test]
fn model_fallback_wire_shapes_are_documented() {
    let params: ModelFallbackParams = serde_json::from_value(json!({
        "session": SessionId::new(),
        "root_session": SessionId::new(),
        "message": MessageId::new(),
        "model": "a/b",
        "error": {"class": "unknown_model", "message": "unknown provider for model: a/b"},
        "attempt": 1,
        "tried": ["a/b"],
    }))
    .unwrap();
    assert_eq!(params.agent, None);
    assert_eq!(
        serde_json::from_value::<ModelFallbackOutcomeWire>(
            json!({"outcome": "retry", "model": "c/d"})
        )
        .unwrap(),
        ModelFallbackOutcomeWire::Retry {
            model: ModelRef::new("c/d")
        }
    );
    assert_eq!(
        serde_json::from_value::<ModelFallbackOutcomeWire>(json!({"outcome": "give_up"})).unwrap(),
        ModelFallbackOutcomeWire::GiveUp
    );
}

/// The plugin receives the documented params and its `retry` model reaches
/// the engine-facing outcome.
#[tokio::test]
async fn model_fallback_params_reach_plugin_and_retry_propagates() {
    let reply = "{'outcome': 'retry', 'model': 'next/' + p['agent'] + '/' + p['error']['class'] \
                 + '/' + str(p['attempt']) + '/' + str(len(p['tried'])) + '/' + p['model'] \
                 + '/' + str(p['root_session'] != p['session'])}";
    let host = PluginHost::connect_all(vec![plugin("chooser", Some(reply))], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let outcome = host.model_fallback(input(SessionId::new())).await;
    assert_eq!(
        outcome,
        ModelFallbackOutcome::Retry {
            model: ModelRef::new("next/explore/retryable/2/2/anthropic/claude-opus-5-5/True")
        }
    );
}

/// Chain rule: `give_up`, errors, malformed replies, and empty models pass
/// the consult on; the first `retry` in load order wins.
#[tokio::test]
async fn model_fallback_first_retry_wins_and_failures_fail_open() {
    let host = PluginHost::connect_all(
        vec![
            plugin("gives-up", Some("{'outcome': 'give_up'}")),
            plugin("errors", None),
            plugin("malformed", Some("{'outcome': 'maybe'}")),
            plugin("empty", Some("{'outcome': 'retry', 'model': ' '}")),
            plugin(
                "first",
                Some("{'outcome': 'retry', 'model': 'first/model'}"),
            ),
            plugin(
                "second",
                Some("{'outcome': 'retry', 'model': 'second/model'}"),
            ),
        ],
        host_info(),
    )
    .await;
    assert_eq!(host.len(), 6, "fixtures must connect");

    assert_eq!(
        host.model_fallback(input(SessionId::new())).await,
        ModelFallbackOutcome::Retry {
            model: ModelRef::new("first/model")
        }
    );
}

/// Every plugin giving up (or failing) gives up.
#[tokio::test]
async fn model_fallback_all_give_up_gives_up() {
    let host = PluginHost::connect_all(
        vec![
            plugin("gives-up", Some("{'outcome': 'give_up'}")),
            plugin("errors", None),
        ],
        host_info(),
    )
    .await;
    assert_eq!(host.len(), 2, "fixtures must connect");
    assert_eq!(
        host.model_fallback(input(SessionId::new())).await,
        ModelFallbackOutcome::GiveUp
    );
}
