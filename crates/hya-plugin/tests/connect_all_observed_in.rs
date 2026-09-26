//! Integration test for `hya-plugin`: `PluginHost::connect_all_observed_in`
//! spawns every plugin in a given directory rather than the host's own cwd.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{HostInfo, PluginKindWire};
use hya_proto::SessionId;
use hya_tool::{
    FormatterPlane, InteractionPlane, LspPlane, PermissionPlane, PermissionRules, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, WebSearchPlane, handle::ArtifactPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const FIXTURE: &str = r#"
import json, os, sys
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    method = msg.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "cwd-fixture", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [{
                "name": "where",
                "description": "Report cwd and env",
                "inputSchema": {"type": "object"},
            }],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif method == "tool/call":
        if (msg.get("params") or {}).get("input", {}).get("exit"):
            sys.exit(0)
        result = {
            "ok": True,
            "output": {"cwd": os.getcwd(), "home": os.environ.get("HOME")},
            "time_ms": 1,
        }
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}), flush=True)
    elif "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}), flush=True)
"#;

fn ctx_with(session: SessionId) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission: permission.for_session(session),
        interaction: interaction.for_session(session),
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        formatter: FormatterPlane::default(),
        agents: Default::default(),
        lsp: LspPlane::default(),
        workdir: PathBuf::from("."),
        roots: vec![PathBuf::from(".")],
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn connect_all_observed_in_spawns_relative_command_in_given_cwd_with_inherited_env() {
    let plugin_dir = std::env::temp_dir().join(format!(
        "hya-plugin-connect-in-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // Mirror a real layout: the plugin script lives under `<project root>/.hya/plugins/x/`.
    let script_dir = plugin_dir.join(".hya/plugins/x");
    std::fs::create_dir_all(&script_dir).unwrap();
    std::fs::write(script_dir.join("script.py"), FIXTURE).unwrap();

    let spec = PluginSpec {
        id: "cwd-fixture".to_string(),
        kind: PluginKindWire::Rust,
        // Relative command: it only resolves when the child's cwd is `script_dir`.
        command: vec!["python3".to_string(), "script.py".to_string()],
        timeout_ms: Some(3000),
        env: BTreeMap::new(),
        posture_overrides: BTreeMap::new(),
        plugin_dir: None,
    };

    let (host, failures) = PluginHost::connect_all_observed_in(
        vec![spec],
        script_dir.clone(),
        HostInfo {
            name: "hya".to_string(),
            version: "0.0.0".to_string(),
        },
    )
    .await;
    assert!(failures.is_empty(), "connect failures: {failures:?}");
    assert_eq!(host.len(), 1, "fixture must connect from its own directory");

    let tools = host.tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "where");

    let session = SessionId::new();
    let out = tools[0]
        .execute(&ctx_with(session), json!({}))
        .await
        .unwrap();

    let expected_cwd = std::fs::canonicalize(&script_dir).unwrap();
    assert_eq!(
        out["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref()),
        "child process cwd must be the given directory"
    );
    let expected_home = std::env::var("HOME").ok();
    assert_eq!(
        out["home"].as_str().map(str::to_string),
        expected_home,
        "the standard environment (HOME) must be inherited, not cleared"
    );

    let _ = std::fs::remove_dir_all(plugin_dir);
}

/// A crashed plugin respawns in the same directory it was first spawned in,
/// so a relative command keeps resolving after a restart.
#[tokio::test]
async fn a_respawned_plugin_keeps_its_given_cwd() {
    let plugin_dir = std::env::temp_dir().join(format!(
        "hya-plugin-respawn-in-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let script_dir = plugin_dir.join(".hya/plugins/x");
    std::fs::create_dir_all(&script_dir).unwrap();
    std::fs::write(script_dir.join("script.py"), FIXTURE).unwrap();
    let spec = PluginSpec {
        id: "cwd-fixture".to_string(),
        kind: PluginKindWire::Rust,
        command: vec!["python3".to_string(), "script.py".to_string()],
        timeout_ms: Some(3000),
        env: BTreeMap::new(),
        posture_overrides: BTreeMap::new(),
        plugin_dir: None,
    };
    let (host, failures) = PluginHost::connect_all_observed_in(
        vec![spec],
        script_dir.clone(),
        HostInfo {
            name: "hya".to_string(),
            version: "0.0.0".to_string(),
        },
    )
    .await;
    assert!(failures.is_empty(), "connect failures: {failures:?}");
    let tool = host.tools().remove(0);
    let ctx = ctx_with(SessionId::new());

    assert!(
        tool.execute(&ctx, json!({"exit": true})).await.is_err(),
        "the child exits without replying"
    );
    let out = tool
        .execute(&ctx, json!({}))
        .await
        .expect("the next call respawns the plugin");
    let expected_cwd = std::fs::canonicalize(&script_dir).unwrap();
    assert_eq!(
        out["cwd"].as_str(),
        Some(expected_cwd.to_string_lossy().as_ref()),
        "the respawned child runs in the given directory"
    );

    let _ = std::fs::remove_dir_all(plugin_dir);
}
