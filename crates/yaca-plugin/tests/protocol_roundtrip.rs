#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;
use yaca_plugin::messages::{
    EventNotificationParams, HookName, HookPosture, HookRegistration, InitializeResult, PluginInfo,
    PluginKindWire, ToolCallParams, ToolCallReply, ToolInfo,
};
use yaca_plugin::protocol::{Frame, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use yaca_proto::{Envelope, Event, EventSeq, MessageId, Role, SessionId, ToolCallId};

fn reparse<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    serde_json::from_str(&serde_json::to_string(value).unwrap()).unwrap()
}

#[test]
fn request_response_notification_roundtrip() {
    let req = JsonRpcRequest::new(
        7,
        "hook/tool.execute.before",
        json!({"args": {"cmd": "ls"}}),
    );
    let s = serde_json::to_string(&req).unwrap();
    assert!(s.contains("\"jsonrpc\":\"2.0\""));
    assert_eq!(req, reparse(&req));

    let ok = JsonRpcResponse::ok(7, json!({"outcome": "continue"}));
    assert_eq!(ok, reparse(&ok));

    let err = JsonRpcResponse::err(7, -32601, "method not found");
    assert_eq!(err.error.as_ref().unwrap().code, -32601);
    assert_eq!(err, reparse(&err));

    let note = JsonRpcNotification::new("event", json!({}));
    let ns = serde_json::to_string(&note).unwrap();
    assert!(!ns.contains("\"id\""));
    assert_eq!(note, reparse(&note));
}

#[test]
fn frame_parse_classifies_by_shape() {
    assert!(matches!(
        Frame::parse(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#).unwrap(),
        Frame::Request(_)
    ));
    assert!(matches!(
        Frame::parse(r#"{"jsonrpc":"2.0","method":"event","params":{}}"#).unwrap(),
        Frame::Notification(_)
    ));
    assert!(matches!(
        Frame::parse(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).unwrap(),
        Frame::Response(_)
    ));
    assert!(Frame::parse("garbage").is_err());
    assert!(Frame::parse(r#"{"jsonrpc":"2.0"}"#).is_err());
}

#[test]
fn hook_name_uses_dotted_wire_form() {
    assert_eq!(
        serde_json::to_string(&HookName::ToolExecuteBefore).unwrap(),
        "\"tool.execute.before\""
    );
    assert_eq!(
        HookName::from_wire("permission.ask"),
        Some(HookName::PermissionAsk)
    );
    assert_eq!(HookName::from_wire("nope"), None);
    assert_eq!(
        HookName::ToolExecuteBefore.method(),
        "hook/tool.execute.before"
    );
    assert_eq!(HookName::PermissionAsk.default_posture(), HookPosture::Safe);
    assert_eq!(HookName::ChatParams.default_posture(), HookPosture::Open);
}

#[test]
fn tool_info_wire_key_is_camel_case() {
    let info = ToolInfo {
        name: "remember".into(),
        description: "remember a fact".into(),
        input_schema: json!({"type": "object"}),
    };
    let s = serde_json::to_string(&info).unwrap();
    assert!(
        s.contains("\"inputSchema\""),
        "wire key must be inputSchema: {s}"
    );
    assert!(!s.contains("input_schema"));
    assert_eq!(info, reparse(&info));
}

#[test]
fn initialize_result_roundtrip() {
    let init = InitializeResult {
        protocol_version: 1,
        plugin: PluginInfo {
            id: "ex".into(),
            version: "0.1.0".into(),
            kind: PluginKindWire::Rust,
        },
        hooks: vec![HookRegistration {
            name: HookName::Event,
            posture: Some(HookPosture::Open),
        }],
        tools: vec![ToolInfo {
            name: "t".into(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
        }],
    };
    assert_eq!(init, reparse(&init));
    assert!(
        serde_json::to_string(&init)
            .unwrap()
            .contains("\"kind\":\"rust\"")
    );
}

#[test]
fn tool_call_roundtrip() {
    let params = ToolCallParams {
        tool: "remember".into(),
        session: SessionId::new(),
        call: ToolCallId::new(),
        input: json!({"k": "v"}),
    };
    assert_eq!(params, reparse(&params));

    let reply = ToolCallReply {
        ok: true,
        output: json!({"ok": true}),
        time_ms: Some(3),
    };
    assert_eq!(reply, reparse(&reply));
}

#[test]
fn event_notification_roundtrip() {
    let params = EventNotificationParams {
        envelope: Envelope {
            seq: EventSeq(1),
            ts_millis: 123,
            event: Event::MessageStarted {
                session: SessionId::new(),
                message: MessageId::new(),
                role: Role::Assistant,
            },
        },
    };
    assert_eq!(params, reparse(&params));
}
