//! Integration tests for `hya-plugin`: the `permission.approve` hook of a
//! session permission mode, on the wire and through `PluginHost`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use hya_core::hooks::{HookDispatcher, PermissionApproveInput};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{
    HookName, HookPosture, HostInfo, PermissionApproveParams, PermissionOutcomeWire,
    PluginKindWire, WireResource,
};
use hya_proto::{AgentName, SessionId};
use hya_tool::{Action, Decision, Resource};
use serde_json::json;

fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: "0.0.0".to_string(),
    }
}

/// A python plugin registering `permission.approve` that answers with
/// `reply` (a Python expression over `p`, the params) or, when `reply` is
/// `None`, with a JSON-RPC error.
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
            "hooks": [{{"name": "permission.approve"}}],
            "tools": [],
        }}
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": result}}), flush=True)
    elif msg.get("method") == "hook/permission.approve":
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

fn input(action: Action, resource: Resource) -> PermissionApproveInput {
    PermissionApproveInput {
        session: SessionId::new(),
        root_session: SessionId::new(),
        agent: Some(AgentName::new("build")),
        mode: "careful".to_string(),
        action,
        resource,
    }
}

#[test]
fn permission_approve_name_round_trips_and_defaults_safe() {
    let name = HookName::PermissionApprove;
    assert_eq!(
        serde_json::to_string(&name).unwrap(),
        "\"permission.approve\""
    );
    assert_eq!(HookName::from_wire("permission.approve"), Some(name));
    assert_eq!(name.as_str(), "permission.approve");
    assert_eq!(name.method(), "hook/permission.approve");
    assert_eq!(name.default_posture(), HookPosture::Safe);
}

#[test]
fn permission_approve_wire_shapes_are_documented() {
    let session = SessionId::new();
    let params = PermissionApproveParams {
        session,
        root_session: session,
        agent: None,
        mode: "careful".to_string(),
        action: Action::Bash,
        resource: WireResource::Command {
            value: "git status".to_string(),
        },
    };
    let value = serde_json::to_value(&params).unwrap();
    assert_eq!(value["mode"], json!("careful"));
    assert_eq!(value["action"], json!("bash"));
    assert_eq!(
        value["resource"],
        json!({"type": "command", "value": "git status"})
    );
    assert!(value.get("agent").is_none(), "absent agent is omitted");
    assert_eq!(
        serde_json::from_value::<PermissionApproveParams>(value).unwrap(),
        params
    );
}

/// The plugin receives the documented params and its answer reaches the
/// engine-facing decision.
#[tokio::test]
async fn permission_approve_params_reach_plugin_and_answers_map() {
    let reply = "({'outcome': 'allow_once'} if p['mode'] == 'careful' and p['agent'] == 'build' \
                 and p['action'] == 'bash' and p['resource']['value'] == 'ls' \
                 and p['root_session'] != p['session'] \
                 else {'outcome': 'reject', 'feedback': 'mode ' + p['mode'] + ' says no'})";
    let host = PluginHost::connect_all(vec![plugin("approver", Some(reply))], host_info()).await;
    assert_eq!(host.len(), 1, "fixture must connect");

    assert_eq!(
        host.permission_approve(input(Action::Bash, Resource::Command("ls".into())))
            .await,
        Some(Decision::AllowOnce)
    );
    // Through the engine-facing dispatcher trait as well.
    assert_eq!(
        HookDispatcher::permission_approve(&host, input(Action::Edit, Resource::Path("x".into())))
            .await,
        Some(Decision::Reject {
            feedback: Some("mode careful says no".to_string())
        })
    );
}

/// `defer`, errors, and malformed replies all fall through (`None`), so the
/// engine asks the user.
#[tokio::test]
async fn permission_approve_defer_and_failures_fall_through() {
    for (id, reply) in [
        ("defers", Some("{'outcome': 'defer'}")),
        ("errors", None),
        ("malformed", Some("{'outcome': 'maybe'}")),
    ] {
        let host = PluginHost::connect_all(vec![plugin(id, reply)], host_info()).await;
        assert_eq!(host.len(), 1, "fixture {id} must connect");
        assert_eq!(
            host.permission_approve(input(Action::Bash, Resource::Command("ls".into())))
                .await,
            None,
            "{id}"
        );
    }
    assert_eq!(
        serde_json::from_value::<PermissionOutcomeWire>(json!({"outcome": "allow_always"}))
            .unwrap(),
        PermissionOutcomeWire::AllowAlways
    );
}
