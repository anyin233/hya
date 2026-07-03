#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};
use hya_proto::SessionId;
use hya_tool::{
    FormatterPlane, InteractionPlane, LspPlane, PermissionPlane, PermissionRules, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, WebSearchPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn tool_fixture() -> Vec<String> {
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
            "plugin": {"id": "toolbox", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [{
                "name": "remember",
                "description": "Remember a fact",
                "inputSchema": {
                    "type": "object",
                    "properties": {"value": {"type": "string"}},
                    "required": ["value"]
                },
            }],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "tool/call":
        params = msg["params"]
        result = {
            "ok": True,
            "output": {
                "tool": params["tool"],
                "value": params["input"]["value"],
                "session": params["session"],
            },
            "time_ms": 4,
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;
    vec!["python3".to_string(), "-c".to_string(), script.to_string()]
}

fn spec() -> PluginSpec {
    PluginSpec {
        id: "toolbox".to_string(),
        kind: PluginKindWire::Rust,
        command: tool_fixture(),
        timeout_ms: Some(3000),
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

fn ctx_with(session: SessionId) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        permission: permission.for_session(session),
        interaction: interaction.for_session(session),
        spawner,
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        formatter: FormatterPlane::default(),
        agents: hya_tool::AgentCatalogPlane::default(),
        lsp: LspPlane::default(),
        workdir: PathBuf::from("."),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn declared_plugin_tool_is_callable() {
    let host = PluginHost::connect_all(vec![spec()], host_info()).await;
    let tools = host.tools();

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "remember");
    assert_eq!(tools[0].schema().description, "Remember a fact");

    let session = SessionId::new();
    let out = tools[0]
        .execute(&ctx_with(session), json!({"value": "ship it"}))
        .await
        .unwrap();

    assert_eq!(out["tool"], "remember");
    assert_eq!(out["value"], "ship it");
    assert_eq!(out["session"], serde_json::to_value(session).unwrap());
}
