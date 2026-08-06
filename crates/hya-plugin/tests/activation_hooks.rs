//! Integration tests for `hya-plugin`: activation hooks.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_core::hooks::{
    HookDispatcher, ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome, ToolOutcomeNative,
};
use hya_plugin::ActivationHookDispatcher;
use hya_plugin::client::PluginClient;
use hya_plugin::messages::{
    EventNotificationParams, HookName, HookPosture, HookRegistration, METHOD_EVENT,
    ToolAfterOutcomeWire, ToolBeforeOutcomeWire, ToolExecuteAfterParams, ToolExecuteBeforeParams,
    WireToolResult,
};
use hya_plugin::protocol::Frame;
use hya_proto::{Envelope, Event, EventSeq, MessageId, SessionId, ToolCallId};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};

#[tokio::test]
async fn activation_dispatcher_routes_declared_tool_hooks_and_exact_event() {
    let session = SessionId::new();
    let message = MessageId::new();
    let call = ToolCallId::new();
    let before_input = json!({"original": "value"});
    let before_mutated = json!({"original": "value", "activation_before": true});
    let after_input = before_mutated.clone();
    let event = Envelope {
        seq: EventSeq(9),
        ts_millis: 42,
        event: Event::SessionTitled {
            session,
            title: "activation hook test".to_string(),
        },
    };

    let registrations = [
        HookRegistration {
            name: HookName::ToolExecuteBefore,
            posture: Some(HookPosture::Safe),
        },
        HookRegistration {
            name: HookName::ToolExecuteAfter,
            posture: Some(HookPosture::Open),
        },
        HookRegistration {
            name: HookName::Event,
            posture: None,
        },
    ];

    let (client_io, server_io) = duplex(4096);
    let (client_read, client_write) = split(client_io);
    let (server_read, mut server_write) = split(server_io);
    let client = PluginClient::new(client_read, client_write);

    let expected_before = ToolExecuteBeforeParams {
        session,
        message,
        call,
        tool: "activation-tool".to_string(),
        input: before_input.clone(),
    };
    let expected_after = ToolExecuteAfterParams {
        session,
        message,
        call,
        tool: "activation-tool".to_string(),
        input: after_input,
        result: WireToolResult::Ok {
            output: json!({"raw": true}),
            time_ms: 17,
        },
    };
    let expected_event = event.clone();

    let server = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();

        let line = lines.next_line().await.unwrap().unwrap();
        let request = match Frame::parse(&line).unwrap() {
            Frame::Request(request) => request,
            other => panic!("tool.execute.before must be a request: {other:?}"),
        };
        assert_eq!(request.method, HookName::ToolExecuteBefore.method());
        assert_eq!(
            request.params,
            serde_json::to_value(expected_before).unwrap()
        );
        let before_reply = json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": serde_json::to_value(ToolBeforeOutcomeWire::Continue {
                input: before_mutated,
            })
            .unwrap(),
        });
        server_write
            .write_all(serde_json::to_string(&before_reply).unwrap().as_bytes())
            .await
            .unwrap();
        server_write.write_all(b"\n").await.unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let request = match Frame::parse(&line).unwrap() {
            Frame::Request(request) => request,
            other => panic!("tool.execute.after must be a request: {other:?}"),
        };
        assert_eq!(request.method, HookName::ToolExecuteAfter.method());
        assert_eq!(
            request.params,
            serde_json::to_value(expected_after).unwrap()
        );
        let after_reply = json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": serde_json::to_value(ToolAfterOutcomeWire::Continue {
                result: WireToolResult::Ok {
                    output: json!({"rewritten": true}),
                    time_ms: 99,
                },
            })
            .unwrap(),
        });
        server_write
            .write_all(serde_json::to_string(&after_reply).unwrap().as_bytes())
            .await
            .unwrap();
        server_write.write_all(b"\n").await.unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let raw: serde_json::Value = serde_json::from_str(&line).unwrap();
        let notification = match Frame::parse(&line).unwrap() {
            Frame::Notification(notification) => notification,
            other => panic!("event must be a notification: {other:?}"),
        };
        assert_eq!(notification.method, METHOD_EVENT);
        assert!(raw.get("id").is_none());
        assert!(raw.get("result").is_none());
        let params: EventNotificationParams = serde_json::from_value(notification.params).unwrap();
        assert_eq!(params.envelope, expected_event);
    });

    let dispatcher = ActivationHookDispatcher::new(client, &registrations);
    let input = match dispatcher
        .tool_execute_before(ToolExecuteBeforeInput {
            session,
            message,
            call,
            tool: "activation-tool".to_string(),
            input: before_input,
        })
        .await
    {
        ToolExecuteBeforeOutcome::Continue { input } => input,
        ToolExecuteBeforeOutcome::Veto { reason } => panic!("unexpected veto: {reason}"),
    };
    assert_eq!(
        input,
        json!({"original": "value", "activation_before": true})
    );

    let ToolExecuteAfterOutcome::Continue { result } = dispatcher
        .tool_execute_after(ToolExecuteAfterInput {
            session,
            message,
            call,
            tool: "activation-tool".to_string(),
            input,
            result: ToolOutcomeNative::Ok {
                output: json!({"raw": true}),
                time_ms: 17,
            },
        })
        .await;
    match result {
        ToolOutcomeNative::Ok { output, time_ms } => {
            assert_eq!(output, json!({"rewritten": true}));
            assert_eq!(time_ms, 99);
        }
        ToolOutcomeNative::Err { message } => panic!("unexpected tool error: {message}"),
    }

    dispatcher.dispatch_event(&event);
    server.await.unwrap();
}

#[tokio::test]
async fn activation_hook_transport_loss_reports_unhealthy() {
    let session = SessionId::new();
    let message = MessageId::new();
    let call = ToolCallId::new();
    let input = json!({"value": true});
    let registrations = [HookRegistration {
        name: HookName::ToolExecuteBefore,
        posture: Some(HookPosture::Safe),
    }];

    let (client_io, server_io) = duplex(4096);
    let (client_read, client_write) = split(client_io);
    let (server_read, server_write) = split(server_io);
    let client = PluginClient::new(client_read, client_write);
    let expected = ToolExecuteBeforeParams {
        session,
        message,
        call,
        tool: "activation-tool".to_string(),
        input: input.clone(),
    };

    let server = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let request = match Frame::parse(&line).unwrap() {
            Frame::Request(request) => request,
            other => panic!("tool.execute.before must be a request: {other:?}"),
        };
        assert_eq!(request.method, HookName::ToolExecuteBefore.method());
        assert_eq!(request.params, serde_json::to_value(expected).unwrap());
        drop(server_write);
    });

    let dispatcher = ActivationHookDispatcher::new(client, &registrations);
    let outcome = dispatcher
        .tool_execute_before(ToolExecuteBeforeInput {
            session,
            message,
            call,
            tool: "activation-tool".to_string(),
            input,
        })
        .await;
    assert!(matches!(outcome, ToolExecuteBeforeOutcome::Veto { .. }));
    server.await.unwrap();
    assert!(!dispatcher.is_healthy());
}
