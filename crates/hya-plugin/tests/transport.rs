//! Integration tests for `hya-plugin`: transport.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;
use std::{collections::BTreeMap, time::SystemTime};

use hya_plugin::PluginError;
use hya_plugin::client::{DEFAULT_CALL_TIMEOUT, PluginClient};
use hya_plugin::messages::{
    ActivationLifecycle, ActivationMetadata, EventNotificationParams, HookName, HostInfo,
    METHOD_EVENT, METHOD_INITIALIZE, METHOD_TOOL_CALL, PluginKindWire, ToolCallParams,
    ToolCallReply,
};
use hya_plugin::protocol::Frame;
use hya_proto::{Envelope, Event, EventSeq, MessageId, Role, SessionId, ToolCallId};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};

const FIXTURE: &str = r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if req.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "fixture", "version": "0.1.0", "kind": "rust"},
            "hooks": [{"name": "tool.execute.before", "posture": "safe"}, {"name": "event"}],
            "tools": [{"name": "remember", "description": "", "inputSchema": {"type": "object"}}],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
"#;

const SHUTDOWN_FIXTURE: &str = r#"
import json, os, sys
sentinel = os.environ["HYA_SHUTDOWN_SENTINEL"]
for line in sys.stdin:
    req = json.loads(line)
    if req.get("method") == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "shutdown-fixture", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif req.get("method") == "shutdown":
        with open(sentinel, "w", encoding="utf-8") as f:
            f.write("shutdown")
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": True}), flush=True)
        break
"#;

const BUNDLE_PROCESS_FIXTURE: &str = r#"
import json, os, sys
stderr_payload = ("E" * (70 * 1024)) + "STDERR_TAIL_SENTINEL"
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "bundle-fixture", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [],
            "workspaceAdapters": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif method == "fixture/info":
        sys.stderr.write(stderr_payload)
        sys.stderr.flush()
        result = {"pid": os.getpid(), "cwd": os.getcwd()}
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif method == "fixture/exit":
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": True}), flush=True)
        break
    elif method == "shutdown":
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": True}), flush=True)
        break
"#;

const BUNDLE_TIMEOUT_FIXTURE: &str = r#"
import json, sys
initialized = False
for line in sys.stdin:
    req = json.loads(line)
    if not initialized and req.get("method") == "initialize":
        initialized = True
        result = {
            "protocol_version": 1,
            "plugin": {"id": "bundle-timeout-fixture", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [],
            "workspaceAdapters": [],
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    else:
        initialized = True
"#;

const STDERR_TAIL_SENTINEL: &str = "STDERR_TAIL_SENTINEL";

#[tokio::test]
async fn demuxes_responses_by_id() {
    let (client_io, server_io) = duplex(4096);
    let (client_read, client_write) = split(client_io);
    let (server_read, mut server_write) = split(server_io);
    let client = PluginClient::new(client_read, client_write);

    let server = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let first: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let second: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let second_resp = json!({"jsonrpc":"2.0","id": second["id"], "result": {"second": true}});
        let first_resp = json!({"jsonrpc":"2.0","id": first["id"], "result": {"first": true}});
        server_write
            .write_all(format!("{second_resp}\n{first_resp}\n").as_bytes())
            .await
            .unwrap();
    });

    let first = client.call("first", json!({}), DEFAULT_CALL_TIMEOUT);
    let second = client.call("second", json!({}), DEFAULT_CALL_TIMEOUT);
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.unwrap(), json!({"first": true}));
    assert_eq!(second.unwrap(), json!({"second": true}));
    server.await.unwrap();
}

#[tokio::test]
async fn bundle_client_tool_call_uses_existing_request_reply_path() {
    let session = SessionId::new();
    let call = ToolCallId::new();
    let (client_io, server_io) = duplex(4096);
    let (client_read, client_write) = split(client_io);
    let (server_read, mut server_write) = split(server_io);
    let client = PluginClient::new(client_read, client_write);

    let server = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["jsonrpc"], "2.0");
        assert_eq!(request["method"], METHOD_TOOL_CALL);
        let id = request.get("id").cloned().unwrap();
        assert!(!id.is_null());
        let params: ToolCallParams = serde_json::from_value(request["params"].clone()).unwrap();
        assert_eq!(params.tool, "echo");
        assert_eq!(params.session, session);
        assert_eq!(params.call, call);
        assert_eq!(params.input, json!({"text": "hello"}));

        let result = ToolCallReply {
            ok: true,
            output: json!({"echoed": "hello"}),
            time_ms: Some(7),
        };
        let response = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": serde_json::to_value(result).unwrap(),
        });
        server_write
            .write_all(format!("{response}\n").as_bytes())
            .await
            .unwrap();
    });

    let reply = client
        .call_tool("echo", session, call, json!({"text": "hello"}))
        .await
        .unwrap();
    assert!(reply.ok);
    assert_eq!(reply.output, json!({"echoed": "hello"}));
    assert_eq!(reply.time_ms, Some(7));
    server.await.unwrap();
}

#[tokio::test]
async fn returns_timeout_errors() {
    let (client_io, _server_io) = duplex(4096);
    let (client_read, client_write) = split(client_io);
    let client = PluginClient::new(client_read, client_write);
    let result = client
        .call("slow", json!({}), Duration::from_millis(10))
        .await;
    assert!(matches!(result, Err(PluginError::Timeout { method }) if method == "slow"));
}

#[tokio::test]
async fn rpc_error_reply_maps_to_rpc_error() {
    let (client_io, server_io) = duplex(4096);
    let (client_read, client_write) = split(client_io);
    let (server_read, mut server_write) = split(server_io);
    let client = PluginClient::new(client_read, client_write);
    tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let req: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        let resp =
            json!({"jsonrpc":"2.0","id": req["id"], "error": {"code": -32601, "message": "nope"}});
        server_write
            .write_all(format!("{resp}\n").as_bytes())
            .await
            .unwrap();
    });
    let result = client.call("x", json!({}), DEFAULT_CALL_TIMEOUT).await;
    assert!(matches!(result, Err(PluginError::Rpc { code: -32601, .. })));
}

#[tokio::test]
async fn handshake_with_fixture_reports_hooks_and_tools() {
    let command = vec!["python3".to_string(), "-c".to_string(), FIXTURE.to_string()];
    let (client, _guard) = PluginClient::spawn(&command, None).unwrap();
    let init = client
        .initialize(HostInfo {
            name: "hya".to_string(),
            version: "0.0.0".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(init.protocol_version, 1);
    assert_eq!(init.plugin.id, "fixture");
    assert_eq!(init.plugin.kind, PluginKindWire::Rust);
    let hook_names: Vec<HookName> = init.contributions.hooks.iter().map(|h| h.name).collect();
    assert!(hook_names.contains(&HookName::ToolExecuteBefore));
    assert!(hook_names.contains(&HookName::Event));
    assert_eq!(init.contributions.tools.len(), 1);
    assert_eq!(init.contributions.tools[0].name, "remember");
}

#[tokio::test]
async fn child_guard_sends_shutdown_before_terminating_child() {
    let dir = std::env::temp_dir().join(format!(
        "hya-plugin-shutdown-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let sentinel = dir.join("shutdown.txt");
    let mut env = BTreeMap::new();
    env.insert(
        "HYA_SHUTDOWN_SENTINEL".to_string(),
        sentinel.to_string_lossy().into_owned(),
    );
    let command = vec![
        "python3".to_string(),
        "-c".to_string(),
        SHUTDOWN_FIXTURE.to_string(),
    ];
    let (client, guard) = PluginClient::spawn(&command, Some(&env)).unwrap();
    client
        .initialize(HostInfo {
            name: "hya".to_string(),
            version: "0.0.0".to_string(),
        })
        .await
        .unwrap();

    drop(guard);

    tokio::time::timeout(Duration::from_secs(2), async {
        while !sentinel.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "shutdown");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn bundle_process_uses_activation_cwd_and_shutdown_reaps_with_bounded_stderr() {
    let dir = std::env::temp_dir().join(format!(
        "hya-plugin-bundle-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let command = vec![
        "python3".to_string(),
        "-c".to_string(),
        BUNDLE_PROCESS_FIXTURE.to_string(),
    ];
    let (client, mut guard) = PluginClient::spawn_bundle(&command, &dir).unwrap();

    let init = client
        .initialize_activation(
            HostInfo {
                name: "hya".to_string(),
                version: "0.34.11-test".to_string(),
            },
            ActivationMetadata {
                activation_id: "activation-test".to_string(),
                lifecycle: ActivationLifecycle::Resident,
            },
        )
        .await
        .unwrap();
    assert_eq!(init.plugin.id, "bundle-fixture");

    let info = client
        .call("fixture/info", json!({}), DEFAULT_CALL_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(info["pid"].as_u64(), guard.pid().map(u64::from));
    assert_eq!(info["cwd"].as_str(), Some(dir.to_string_lossy().as_ref()));

    let status = guard.shutdown().await.unwrap();
    assert!(status.success());
    let tail = guard.stderr_tail();
    assert!(tail.len() <= 64 * 1024);
    assert!(tail.ends_with(STDERR_TAIL_SENTINEL.as_bytes()));

    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[tokio::test]
async fn bundle_process_does_not_inherit_host_environment() {
    let dir = std::env::temp_dir().join(format!(
        "hya-plugin-bundle-env-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let command = vec![
        "/usr/bin/env".to_string(),
        "/bin/sh".to_string(),
        "-c".to_string(),
        "if [ -n \"${HOME-}\" ]; then printf 'HOST_HOME_INHERITED\\n' >&2; fi".to_string(),
    ];
    let (_client, mut guard) = PluginClient::spawn_bundle(&command, &dir).unwrap();

    let status = guard.wait_for_exit().await.unwrap();
    let tail = guard.stderr_tail();
    let _ = std::fs::remove_dir_all(dir);

    assert!(status.success());
    assert!(tail.len() <= 64 * 1024);
    assert!(
        tail.is_empty(),
        "bundle stderr: {}",
        String::from_utf8_lossy(&tail)
    );
}

#[tokio::test]
async fn bundle_process_terminate_reaps_without_shutdown() {
    let dir = std::env::temp_dir().join(format!(
        "hya-plugin-bundle-terminate-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let command = vec![
        "python3".to_string(),
        "-c".to_string(),
        BUNDLE_PROCESS_FIXTURE.to_string(),
    ];
    let (client, mut guard) = PluginClient::spawn_bundle(&command, &dir).unwrap();
    client
        .initialize_activation(
            HostInfo {
                name: "hya".to_string(),
                version: "0.34.11-test".to_string(),
            },
            ActivationMetadata {
                activation_id: "activation-test".to_string(),
                lifecycle: ActivationLifecycle::Transient,
            },
        )
        .await
        .unwrap();
    assert!(guard.pid().is_some());

    let status = guard.terminate().await.unwrap();
    assert!(!status.success());
    assert_eq!(guard.pid(), None);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn bundle_process_exit_is_observed_without_transparent_restart() {
    let dir = std::env::temp_dir().join(format!(
        "hya-plugin-bundle-exit-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let command = vec![
        "python3".to_string(),
        "-c".to_string(),
        BUNDLE_PROCESS_FIXTURE.to_string(),
    ];
    let (client, mut guard) = PluginClient::spawn_bundle(&command, &dir).unwrap();
    client
        .initialize_activation(
            HostInfo {
                name: "hya".to_string(),
                version: "0.34.11-test".to_string(),
            },
            ActivationMetadata {
                activation_id: "activation-test".to_string(),
                lifecycle: ActivationLifecycle::Transient,
            },
        )
        .await
        .unwrap();
    let initial_pid = guard.pid().expect("bundle process must have a pid");
    assert!(initial_pid > 0);

    let exit = client
        .call("fixture/exit", json!({}), DEFAULT_CALL_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(exit, json!(true));

    let status = guard.wait_for_exit().await.unwrap();
    assert!(status.success());
    assert_eq!(guard.pid(), None);

    let result = client
        .call("fixture/info", json!({}), DEFAULT_CALL_TIMEOUT)
        .await;
    assert!(matches!(
        result,
        Err(PluginError::Closed | PluginError::Io(_))
    ));

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn bundle_timeout_taints_client_and_prevents_second_rpc() {
    let dir = std::env::temp_dir().join(format!(
        "hya-plugin-bundle-timeout-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let command = vec![
        "python3".to_string(),
        "-c".to_string(),
        BUNDLE_TIMEOUT_FIXTURE.to_string(),
    ];
    let (client, mut guard) = PluginClient::spawn_bundle(&command, &dir).unwrap();
    client
        .initialize_activation(
            HostInfo {
                name: "hya".to_string(),
                version: "0.34.11-test".to_string(),
            },
            ActivationMetadata {
                activation_id: "activation-test".to_string(),
                lifecycle: ActivationLifecycle::Transient,
            },
        )
        .await
        .unwrap();

    let first = client
        .call("fixture/blocked", json!({}), Duration::from_millis(10))
        .await;
    let second = client
        .call("fixture/second", json!({}), Duration::from_millis(10))
        .await;
    let terminate = guard.terminate().await;
    let _ = std::fs::remove_dir_all(dir);

    assert!(terminate.is_ok());
    assert!(matches!(
        first,
        Err(PluginError::Timeout { method }) if method == "fixture/blocked"
    ));
    assert!(matches!(second, Err(PluginError::Closed)));
}

#[tokio::test]
async fn bundle_process_is_per_activation_and_resident_process_is_stable_while_healthy() {
    let root = std::env::temp_dir().join(format!(
        "hya-plugin-bundle-modes-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let transient_a_dir = root.join("transient-a");
    let transient_b_dir = root.join("transient-b");
    let resident_dir = root.join("resident");
    std::fs::create_dir_all(&transient_a_dir).unwrap();
    std::fs::create_dir_all(&transient_b_dir).unwrap();
    std::fs::create_dir_all(&resident_dir).unwrap();

    let command = vec![
        "python3".to_string(),
        "-c".to_string(),
        BUNDLE_PROCESS_FIXTURE.to_string(),
    ];
    let (first, second) = tokio::join!(
        async { PluginClient::spawn_bundle(&command, &transient_a_dir) },
        async { PluginClient::spawn_bundle(&command, &transient_b_dir) },
    );
    let (first_client, mut first_guard) = first.unwrap();
    let (second_client, mut second_guard) = second.unwrap();

    let (first_init, second_init) = tokio::join!(
        first_client.initialize_activation(
            HostInfo {
                name: "hya".to_string(),
                version: "0.34.11-test".to_string(),
            },
            ActivationMetadata {
                activation_id: "activation-transient-a".to_string(),
                lifecycle: ActivationLifecycle::Transient,
            },
        ),
        second_client.initialize_activation(
            HostInfo {
                name: "hya".to_string(),
                version: "0.34.11-test".to_string(),
            },
            ActivationMetadata {
                activation_id: "activation-transient-b".to_string(),
                lifecycle: ActivationLifecycle::Transient,
            },
        ),
    );
    first_init.unwrap();
    second_init.unwrap();
    let first_pid = first_guard
        .pid()
        .expect("first transient process must live");
    let second_pid = second_guard
        .pid()
        .expect("second transient process must live");
    assert_ne!(first_pid, second_pid);

    let (resident_client, mut resident_guard) =
        PluginClient::spawn_bundle(&command, &resident_dir).unwrap();
    resident_client
        .initialize_activation(
            HostInfo {
                name: "hya".to_string(),
                version: "0.34.11-test".to_string(),
            },
            ActivationMetadata {
                activation_id: "activation-resident".to_string(),
                lifecycle: ActivationLifecycle::Resident,
            },
        )
        .await
        .unwrap();
    let resident_pid = resident_guard.pid().expect("resident process must live");
    let first_info = resident_client
        .call("fixture/info", json!({}), DEFAULT_CALL_TIMEOUT)
        .await
        .unwrap();
    let second_info = resident_client
        .call("fixture/info", json!({}), DEFAULT_CALL_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(first_info["pid"].as_u64(), Some(u64::from(resident_pid)));
    assert_eq!(second_info["pid"], first_info["pid"]);

    let (first_status, second_status, resident_status) = tokio::join!(
        first_guard.shutdown(),
        second_guard.shutdown(),
        resident_guard.shutdown(),
    );
    assert!(first_status.unwrap().success());
    assert!(second_status.unwrap().success());
    assert!(resident_status.unwrap().success());

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn bundle_activation_initialize_preserves_method_roles() {
    let (client_io, server_io) = duplex(4096);
    let (client_read, client_write) = split(client_io);
    let (server_read, mut server_write) = split(server_io);
    let client = PluginClient::new(client_read, client_write);

    let server = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        let mut methods = Vec::new();

        let line = lines.next_line().await.unwrap().unwrap();
        let frame = Frame::parse(&line).unwrap();
        let request = match frame {
            Frame::Request(request) => request,
            other => panic!("initialize must be a request: {other:?}"),
        };
        methods.push(request.method.clone());
        assert_eq!(request.method, METHOD_INITIALIZE);
        assert_eq!(
            request.params,
            json!({
                "protocol_version": 1,
                "host": {"name": "hya", "version": "0.34.11-test"},
                "activation_id": "activation-test",
                "lifecycle": "transient"
            })
        );
        let initialize_result = json!({
            "protocol_version": 1,
            "plugin": {"id": "bundle-fixture", "version": "0.1.0", "kind": "rust"},
            "hooks": [],
            "tools": [],
            "workspaceAdapters": []
        });
        let initialize_reply = json!({
            "jsonrpc": "2.0",
            "id": request.id,
            "result": initialize_result
        });
        server_write
            .write_all(format!("{initialize_reply}\n").as_bytes())
            .await
            .unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let frame = Frame::parse(&line).unwrap();
        let request = match frame {
            Frame::Request(request) => request,
            other => panic!("tool call must be a request: {other:?}"),
        };
        methods.push(request.method.clone());
        assert_eq!(request.method, METHOD_TOOL_CALL);
        let reply = json!({"jsonrpc": "2.0", "id": request.id, "result": {}});
        server_write
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let frame = Frame::parse(&line).unwrap();
        let request = match frame {
            Frame::Request(request) => request,
            other => panic!("hook call must be a request: {other:?}"),
        };
        methods.push(request.method.clone());
        assert_eq!(request.method, HookName::ToolExecuteBefore.method());
        let reply = json!({"jsonrpc": "2.0", "id": request.id, "result": {}});
        server_write
            .write_all(format!("{reply}\n").as_bytes())
            .await
            .unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let raw: serde_json::Value = serde_json::from_str(&line).unwrap();
        let frame = Frame::parse(&line).unwrap();
        let notification = match frame {
            Frame::Notification(notification) => notification,
            other => panic!("event must be a notification: {other:?}"),
        };
        methods.push(notification.method.clone());
        assert_eq!(notification.method, METHOD_EVENT);
        assert!(raw.get("id").is_none());
        assert!(raw.get("result").is_none());
        assert!(raw["params"].get("envelope").is_some());

        methods
    });

    let init = client
        .initialize_activation(
            HostInfo {
                name: "hya".to_string(),
                version: "0.34.11-test".to_string(),
            },
            ActivationMetadata {
                activation_id: "activation-test".to_string(),
                lifecycle: ActivationLifecycle::Transient,
            },
        )
        .await
        .unwrap();
    assert_eq!(init.plugin.id, "bundle-fixture");

    client
        .call(METHOD_TOOL_CALL, json!({}), DEFAULT_CALL_TIMEOUT)
        .await
        .unwrap();
    let hook_method = HookName::ToolExecuteBefore.method();
    client
        .call(&hook_method, json!({}), DEFAULT_CALL_TIMEOUT)
        .await
        .unwrap();

    let event = EventNotificationParams {
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
    client
        .notify(METHOD_EVENT, serde_json::to_value(event).unwrap())
        .await
        .unwrap();

    let methods = server.await.unwrap();
    assert_eq!(
        methods,
        vec![
            METHOD_INITIALIZE.to_string(),
            METHOD_TOOL_CALL.to_string(),
            HookName::ToolExecuteBefore.method(),
            METHOD_EVENT.to_string(),
        ]
    );
    assert!(!methods.iter().any(|method| method == "agent/invoke"));
}
