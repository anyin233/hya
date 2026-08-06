//! Integration tests for `hya-plugin`: permission identity.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};
use hya_plugin::{PermissionBridge, PluginHost};
use hya_tool::PermissionInterceptor;

fn fixture(id: &str, version: &str) -> Vec<String> {
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
            "plugin": {{"id": "{id}", "version": "{version}", "kind": "rust"}},
            "hooks": [{{"name": "permission.ask", "posture": "safe"}}],
            "tools": [],
        }}
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": result}}), flush=True)
    elif "id" in msg:
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": {{}}}}), flush=True)
"#
    );
    vec!["python3".to_string(), "-c".to_string(), script]
}

fn spec(id: &str, version: &str) -> PluginSpec {
    PluginSpec {
        id: id.to_string(),
        kind: PluginKindWire::Rust,
        command: fixture(id, version),
        timeout_ms: Some(3_000),
        env: BTreeMap::new(),
        posture_overrides: BTreeMap::new(),
    }
}

fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: "0.0.0".to_string(),
    }
}

async fn bridge(declarations: &[(&str, &str)]) -> PermissionBridge {
    let specs = declarations
        .iter()
        .map(|(id, version)| spec(id, version))
        .collect();
    let host = PluginHost::connect_all(specs, host_info()).await;
    assert_eq!(
        host.len(),
        declarations.len(),
        "fixture plugins must connect"
    );
    PermissionBridge::new(Arc::new(host))
}

#[tokio::test]
async fn permission_bridge_semantic_identity_tracks_deterministic_permission_chain() {
    let baseline_bridge = bridge(&[("alpha", "1.0.0")]).await;
    let Some(baseline) = baseline_bridge.semantic_identity_v1() else {
        panic!("permission bridge must expose an identity");
    };
    assert_ne!(baseline, [0; 32]);

    let same_bridge = bridge(&[("alpha", "1.0.0")]).await;
    assert_eq!(same_bridge.semantic_identity_v1(), Some(baseline));

    let changed_bridge = bridge(&[("alpha", "1.0.1")]).await;
    assert_ne!(changed_bridge.semantic_identity_v1(), Some(baseline));

    let alpha_then_beta = bridge(&[("alpha", "1.0.0"), ("beta", "1.0.0")]).await;
    let beta_then_alpha = bridge(&[("beta", "1.0.0"), ("alpha", "1.0.0")]).await;
    assert_ne!(
        alpha_then_beta.semantic_identity_v1(),
        beta_then_alpha.semantic_identity_v1()
    );
}
