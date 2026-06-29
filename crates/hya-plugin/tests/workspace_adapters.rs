#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};

fn fixture() -> Vec<String> {
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
            "plugin": {"id": "workspace", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [],
            "workspaceAdapters": [{
                "type": "remote",
                "name": "Remote",
                "description": "Remote workspace",
            }],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    vec!["python3".to_string(), "-c".to_string(), script.to_string()]
}

fn spec() -> PluginSpec {
    PluginSpec {
        id: "workspace".to_string(),
        kind: PluginKindWire::Rust,
        command: fixture(),
        timeout_ms: Some(3000),
        env: BTreeMap::new(),
        posture_overrides: BTreeMap::new(),
    }
}

#[tokio::test]
async fn host_exposes_workspace_adapter_metadata() {
    let host = PluginHost::connect_all(
        vec![spec()],
        HostInfo {
            name: "hya".to_string(),
            version: "0.0.0".to_string(),
        },
    )
    .await;

    let adapters = host.workspace_adapters();
    assert_eq!(adapters.len(), 1);
    assert_eq!(adapters[0].r#type, "remote");
    assert_eq!(adapters[0].name, "Remote");
    assert_eq!(adapters[0].description, "Remote workspace");
}
