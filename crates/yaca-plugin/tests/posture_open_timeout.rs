#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Phase 5: an `open`-posture `chat.params` plugin that misses its deadline must
//! fail open — the engine keeps the original `CompletionRequest`, no error.

use std::collections::BTreeMap;

use yaca_core::hooks::{ChatParamsInput, ChatParamsOutcome, HookDispatcher};
use yaca_plugin::PluginHost;
use yaca_plugin::config::PluginSpec;
use yaca_plugin::messages::{HostInfo, PluginKindWire};
use yaca_proto::{MessageId, ModelRef, SessionId};
use yaca_provider::CompletionRequest;

fn slow_chat_params_fixture() -> Vec<String> {
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
            "plugin": {"id": "slow", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "chat.params", "posture": "open"}],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "hook/chat.params":
        time.sleep(2)
        req = msg["params"]["request"]
        req["temperature"] = 0.999
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"outcome": "continue", "request": req}}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    vec!["python3".to_string(), "-c".to_string(), script.to_string()]
}

#[tokio::test]
async fn chat_params_open_timeout_keeps_original_request() {
    let spec = PluginSpec {
        id: "slow".to_string(),
        kind: PluginKindWire::Rust,
        command: slow_chat_params_fixture(),
        timeout_ms: Some(200),
        env: BTreeMap::new(),
        posture_overrides: BTreeMap::new(),
    };
    let host = PluginHost::connect_all(
        vec![spec],
        HostInfo {
            name: "yaca".to_string(),
            version: "0.0.0".to_string(),
        },
    )
    .await;
    assert_eq!(host.len(), 1, "fixture must connect");

    let original = CompletionRequest {
        model: ModelRef::new("test-model"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: Some(0.5),
        max_output_tokens: None,
        reasoning: None,
    };
    let ChatParamsOutcome::Continue { request } = host
        .chat_params(ChatParamsInput {
            session: SessionId::new(),
            message: MessageId::new(),
            request: original,
        })
        .await;

    assert_eq!(
        request.temperature,
        Some(0.5),
        "open timeout must not mutate"
    );
    assert_eq!(request.model, ModelRef::new("test-model"));
}
