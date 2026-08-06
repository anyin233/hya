//! Integration tests for `hya-plugin`: configured id mismatch.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};

fn mismatched_plugin_command() -> Vec<String> {
    vec![
        "python3".to_string(),
        "-c".to_string(),
        r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if req.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "handshake-id", "version": "1", "kind": "rust"},
            "tools": []
        }
        print(json.dumps({"jsonrpc":"2.0", "id":req["id"], "result":result}), flush=True)
"#
        .to_string(),
    ]
}

#[tokio::test]
async fn configured_plugin_id_must_match_handshake_id() {
    let spec = PluginSpec {
        id: "configured-id".to_string(),
        kind: PluginKindWire::Rust,
        command: mismatched_plugin_command(),
        timeout_ms: Some(1_000),
        env: BTreeMap::new(),
        posture_overrides: BTreeMap::new(),
    };
    let host = PluginHost::connect_all(
        vec![spec],
        HostInfo {
            name: "test".to_string(),
            version: "0".to_string(),
        },
    )
    .await;

    assert!(
        host.is_empty(),
        "mismatched handshake identity must be rejected"
    );
}
