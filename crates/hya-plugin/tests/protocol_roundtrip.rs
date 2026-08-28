//! Integration tests for `hya-plugin`: protocol roundtrip.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_plugin::PluginError;
use hya_plugin::messages::{
    EventNotificationParams, HookName, HookPosture, HookRegistration, InitializeResult,
    PluginContributionSet, PluginInfo, PluginKindWire, SkillContribution, ToolCallParams,
    ToolCallReply, ToolInfo, WorkspaceAdapterInfo,
};
use hya_plugin::protocol::{Frame, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use hya_proto::{Envelope, Event, EventSeq, MessageId, Role, SessionId, ToolCallId};
use serde_json::json;

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
fn parse_rejects_non_jsonrpc_2_frames() {
    let frames = [
        r#"{"jsonrpc":"1.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"1.0","method":"event","params":{}}"#,
        r#"{"jsonrpc":"1.0","id":1,"result":{}}"#,
    ];

    for line in frames {
        assert!(Frame::parse(line).is_err());
    }
}

#[test]
fn parse_rejects_response_with_result_and_error() {
    assert!(Frame::parse(
        r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true},"error":{"code":-32600,"message":"invalid request"}}"#
    )
    .is_err());
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
        contributions: PluginContributionSet {
            hooks: vec![HookRegistration {
                name: HookName::Event,
                posture: Some(HookPosture::Open),
            }],
            tools: vec![ToolInfo {
                name: "t".into(),
                description: String::new(),
                input_schema: json!({"type": "object"}),
            }],
            skills: Vec::new(),
            workspace_adapters: vec![WorkspaceAdapterInfo {
                r#type: "folder".into(),
                name: "Folder".into(),
                description: "Local folder workspace".into(),
            }],
        },
    };
    assert_eq!(init, reparse(&init));
    assert!(
        serde_json::to_string(&init)
            .unwrap()
            .contains("\"kind\":\"rust\"")
    );
}

#[test]
fn initialize_result_roundtrips_skill_contribution() {
    let init = InitializeResult {
        protocol_version: 1,
        plugin: PluginInfo {
            id: "skills".into(),
            version: "0.1.0".into(),
            kind: PluginKindWire::Rust,
        },
        contributions: PluginContributionSet {
            hooks: Vec::new(),
            tools: Vec::new(),
            skills: vec![SkillContribution {
                id: "reviewer".into(),
                content: "test".into(),
                digest: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
            }],
            workspace_adapters: Vec::new(),
        },
    };

    assert_eq!(init, reparse(&init));
    let encoded = serde_json::to_string(&init).unwrap();
    assert!(encoded.contains("\"skills\""));
    assert!(encoded.contains("\"id\":\"reviewer\""));
    assert!(encoded.contains(
        "\"digest\":\"9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\""
    ));

    let legacy = json!({
        "protocol_version": 1,
        "plugin": { "id": "legacy", "version": "0.1.0", "kind": "rust" },
        "hooks": [],
        "tools": [],
        "workspaceAdapters": [],
    });
    let decoded: InitializeResult = serde_json::from_value(legacy).unwrap();
    assert!(decoded.contributions.skills.is_empty());
}

#[test]
fn contribution_validation_rejects_malformed_and_duplicate_declarations() {
    let duplicate = PluginContributionSet {
        skills: vec![
            SkillContribution {
                id: "reviewer".into(),
                content: "test".into(),
                digest: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
            },
            SkillContribution {
                id: "reviewer".into(),
                content: "test".into(),
                digest: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
            },
        ],
        ..PluginContributionSet::default()
    };
    assert!(matches!(
        duplicate.validate("duplicate-plugin"),
        Err(PluginError::DuplicateContribution { kind, id, .. })
            if kind == "skill" && id == "reviewer"
    ));

    let malformed = PluginContributionSet {
        skills: vec![SkillContribution {
            id: "reviewer".into(),
            content: "test".into(),
            digest: "A".repeat(64),
        }],
        ..PluginContributionSet::default()
    };
    assert!(matches!(
        malformed.validate("malformed-plugin"),
        Err(PluginError::InvalidContribution { plugin, kind, detail, .. })
            if plugin == "malformed-plugin"
                && kind == "skill"
                && detail.contains("lowercase SHA-256")
    ));

    let mismatch = PluginContributionSet {
        skills: vec![SkillContribution {
            id: "reviewer".into(),
            content: "test".into(),
            digest: "0000000000000000000000000000000000000000000000000000000000000000".into(),
        }],
        ..PluginContributionSet::default()
    };
    assert!(matches!(
        mismatch.validate("mismatch-plugin"),
        Err(PluginError::InvalidContribution { plugin, kind, detail, .. })
            if plugin == "mismatch-plugin"
                && kind == "skill"
                && detail.contains("does not match UTF-8 content")
    ));

    let unknown = json!({
        "protocol_version": 1,
        "plugin": { "id": "strict", "version": "0.1.0", "kind": "rust" },
        "unexpected": true,
    });
    assert!(serde_json::from_value::<InitializeResult>(unknown).is_err());
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
