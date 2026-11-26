//! Core `/v1` HTTP contract: process/config/catalog/logs, session CRUD and
//! error model, event-driven turns and busy/cancel, filesystem endpoints,
//! MCP/project/VCS/workflow/schema surfaces, PTY lifecycle, and pagination.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use hya_proto::FinishReason;
use hya_provider::{FakeProvider, FakeStep};
use hya_server::{AppState, router};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::harness::{
    assert_err, b64_decode, create_session, enc, get_json, id_str, ok_del, ok_get, ok_patch,
    ok_post, post, run_prompt, send_raw, state_with_provider,
};
use crate::support;

async fn hello_state() -> AppState {
    state_with_provider(FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("hello from the agent".into()),
        FakeStep::Finish(FinishReason::Stop),
    ]]))
    .await
}

#[tokio::test]
async fn process_config_catalog_and_logs_answer() {
    let app = router(hello_state().await);
    assert_eq!(ok_get(&app, "/v1/health").await["ok"], json!(true));
    let _ = ok_get(&app, "/v1/location").await;
    assert!(ok_get(&app, "/v1/config").await["values"].is_object());
    assert_eq!(
        ok_patch(
            &app,
            "/v1/config",
            json!({"patch": {"v1Probe": {"nested": true}}})
        )
        .await["values"]["v1Probe"]["nested"],
        json!(true)
    );
    assert!(
        ok_get(&app, "/v1/models").await["models"]
            .as_array()
            .is_some_and(|r| !r.is_empty())
    );
    assert!(
        ok_get(&app, "/v1/tools").await["tools"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == "read"))
    );
    let _ = ok_post(
        &app,
        "/v1/logs",
        json!({"service": "tui", "level": "LOG_LEVEL_INFO", "message": "probe"}),
    )
    .await;
}

#[tokio::test]
async fn session_crud_defaults_and_error_model() {
    let app = router(hello_state().await);
    let workdir = std::env::temp_dir().to_string_lossy().into_owned();

    // Empty agent/model falls back to server default; explicit model keeps default agent.
    assert_eq!(
        ok_post(&app, "/v1/sessions", json!({"workdir": workdir})).await["session"]["agent"],
        json!("build")
    );
    let explicit = ok_post(
        &app,
        "/v1/sessions",
        json!({"workdir": workdir, "model": "other/m"}),
    )
    .await;
    assert_eq!(
        (
            &explicit["session"]["agent"],
            &explicit["session"]["model"]["modelId"]
        ),
        (&json!("build"), &json!("m"))
    );

    // Create, get, patch, list, delete, and 404 after delete.
    let session = create_session(&app, &std::env::temp_dir()).await;
    let uri = format!("/v1/sessions/{session}");
    let got = ok_get(&app, &uri).await;
    assert_eq!(
        (&got["id"], &got["agent"]),
        (&json!(session), &json!("build"))
    );
    assert_eq!(
        ok_patch(&app, &uri, json!({"title": "v1 probe"})).await["title"],
        json!("v1 probe")
    );
    assert!(
        ok_get(&app, "/v1/sessions").await["sessions"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == json!(session)))
    );
    assert_eq!(
        get_json(&app, "/v1/sessions/hysec_00000000000000000000")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let _ = ok_del(&app, &uri).await;
    assert_err(
        get_json(&app, &uri).await,
        StatusCode::NOT_FOUND,
        "session_not_found",
    );

    // Invalid requests render the stable error envelope.
    assert_err(
        post(
            &app,
            "/v1/sessions/not-a-session/turns",
            json!({"prompt": {"text": "x"}}),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );
    assert_err(
        post(&app, "/v1/sessions", json!({"agent": "build"})).await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );
    let _ = ok_get(&app, "/v1/interactions").await;
}

#[tokio::test]
async fn turn_lifecycle_busy_cancel_and_replay() {
    let state = hello_state().await;
    let engine = Arc::clone(&state.engine);
    let app = router(state);
    let session = create_session(&app, &std::env::temp_dir()).await;

    let admitted = ok_post(
        &app,
        &format!("/v1/sessions/{session}/turns"),
        json!({"prompt": {"text": "say hello"}}),
    )
    .await;
    let turn = id_str(&admitted["turn"]["id"]);
    assert_eq!(admitted["turn"]["state"], json!("TURN_STATE_RUNNING"));

    let waited = ok_post(
        &app,
        &format!("/v1/sessions/{session}/turns/{turn}/wait"),
        json!({}),
    )
    .await;
    assert_eq!(
        (&waited["state"], &waited["finish"]),
        (&json!("TURN_STATE_FINISHED"), &json!("FINISH_REASON_STOP"))
    );

    let msgs = ok_get(&app, &format!("/v1/sessions/{session}/messages")).await;
    assert!(msgs["messages"].as_array().unwrap().iter().any(|m| {
        m["parts"].as_array().is_some_and(|p| {
            p.iter()
                .any(|x| x["text"]["text"] == json!("hello from the agent"))
        })
    }));

    let ev = ok_get(&app, &format!("/v1/sessions/{session}/events")).await;
    assert!(ev["events"].as_array().is_some_and(|e| !e.is_empty()));
    let next_seq = ev["nextSeq"]
        .as_str()
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| ev["nextSeq"].as_u64())
        .unwrap_or(0);
    assert!(next_seq > 0);

    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/v1/sessions/{session}/events/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/event-stream")
    );

    // SessionInfo.busy reflects any engine turn lease and cancel stops it.
    let id: hya_proto::SessionId = session.parse().unwrap();
    let uri = format!("/v1/sessions/{session}");
    assert_ne!(ok_get(&app, &uri).await["busy"], json!(true));
    let lease = engine.try_begin_turn(id).unwrap();
    assert_eq!(ok_get(&app, &uri).await["busy"], json!(true));
    assert_eq!(
        ok_post(
            &app,
            &format!("/v1/sessions/{session}/turns/t/cancel"),
            Value::Null
        )
        .await["state"],
        json!("TURN_STATE_CANCELLED")
    );
    drop(lease);
    assert_ne!(ok_get(&app, &uri).await["busy"], json!(true));
}

#[tokio::test]
async fn fs_read_max_bytes_binary_list_find_search_and_traversal_refusal() {
    let app = router(hello_state().await);
    let root = support::tempdir("v1-fs");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "fn alpha() {}\n").unwrap();
    std::fs::write(root.join("notes.txt"), "hello v1 probe\n").unwrap();
    std::fs::write(
        root.join("shot.png"),
        [0x89, b'P', b'N', b'G', 0xff, 0xfe, 0, 1, 2, 3],
    )
    .unwrap();
    let q = enc(&root.to_string_lossy());

    let text = ok_get(&app, &format!("/v1/fs/read?directory={q}&path=notes.txt")).await;
    assert_eq!(text["text"], json!(true));
    assert!(
        String::from_utf8_lossy(&b64_decode(text["content"].as_str().unwrap_or("")))
            .contains("hello v1 probe")
    );

    let bin = ok_get(
        &app,
        &format!("/v1/fs/read?directory={q}&path=shot.png&maxBytes=6"),
    )
    .await;
    assert_eq!(
        b64_decode(bin["content"].as_str().unwrap_or("")),
        vec![0x89, b'P', b'N', b'G', 0xff, 0xfe]
    );
    assert_ne!(bin["text"], json!(true));
    assert_eq!(bin["mime"], json!("image/png"));

    let abs = enc(&root.join("shot.png").to_string_lossy());
    assert_eq!(
        b64_decode(
            ok_get(&app, &format!("/v1/fs/read?directory={q}&path={abs}")).await["content"]
                .as_str()
                .unwrap_or("")
        )
        .len(),
        10
    );

    assert!(
        ok_get(&app, &format!("/v1/fs/list?directory={q}")).await["entries"]
            .as_array()
            .is_some_and(|r| r.iter().any(|e| e["name"] == "src"))
    );
    assert!(
        ok_get(
            &app,
            &format!("/v1/fs/find?directory={q}&pattern={}", enc("**/*.rs"))
        )
        .await["paths"]
            .as_array()
            .is_some_and(|r| r
                .iter()
                .any(|p| p.as_str().is_some_and(|s| s.ends_with("a.rs"))))
    );
    assert!(
        ok_get(
            &app,
            &format!("/v1/fs/search?directory={q}&query={}", enc("v1 probe"))
        )
        .await["matches"]
            .as_array()
            .is_some_and(|r| !r.is_empty())
    );
    assert_err(
        get_json(
            &app,
            &format!("/v1/fs/read?directory={q}&path={}", enc("../../etc/passwd")),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[derive(Default)]
struct RecordingWorkflowControl {
    seen: Mutex<Vec<Option<std::path::PathBuf>>>,
}

impl hya_server::WorkflowControl for RecordingWorkflowControl {
    fn execute(
        &self,
        _: hya_proto::SessionId,
        _: hya_proto::WorkflowCommand,
        _: hya_proto::WorkflowDelivery,
    ) -> hya_server::WorkflowControlFuture<'_> {
        Box::pin(async {
            Err(hya_server::WorkflowControlError::new(
                "UNIMPLEMENTED",
                "unused",
            ))
        })
    }
    fn list(
        &self,
        scope: Option<std::path::PathBuf>,
    ) -> futures::future::BoxFuture<
        '_,
        Result<Vec<hya_proto::WorkflowSummary>, hya_server::WorkflowControlError>,
    > {
        self.seen.lock().unwrap().push(scope);
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[tokio::test]
async fn catalog_mcp_vcs_workflows_pty_and_runtime_schemas() {
    let wf_control = Arc::new(RecordingWorkflowControl::default());
    let mcp_control = crate::mcp_fixture::TestMcpControl::new(std::collections::BTreeMap::from([
        (
            "disabled".into(),
            hya_mcp::McpServerConfig {
                enabled: Some(false),
                ..Default::default()
            },
        ),
        ("failed".into(), hya_mcp::McpServerConfig::default()),
    ]))
    .await;
    let app = router(
        hello_state()
            .await
            .with_mcp_control(mcp_control)
            .with_workflow_control(wf_control.clone()),
    );

    // MCP reports disabled & failed preparations.
    let mcp = ok_get(&app, "/v1/mcp").await;
    assert_eq!(
        (&mcp["servers"][0]["name"], &mcp["servers"][0]["state"]),
        (&json!("disabled"), &json!("MCP_SERVER_STATE_DISCONNECTED"))
    );
    assert_eq!(
        (&mcp["servers"][1]["name"], &mcp["servers"][1]["state"]),
        (&json!("failed"), &json!("MCP_SERVER_STATE_FAILED"))
    );
    assert!(
        mcp["servers"][1]["error"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );

    // Runtime schemas generation-tagged table.
    let schemas = ok_get(&app, "/v1/runtime/schemas").await;
    assert_eq!(schemas["schemas"], json!([]));
    assert!(schemas["generation"].is_u64());

    // Projects ensure + VCS on non-git directory.
    let root = support::tempdir("v1-proj");
    let scope = root.to_string_lossy().into_owned();
    assert!(
        ok_get(&app, "/v1/projects").await["projects"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    let _ = ok_post(&app, "/v1/projects/ensure", json!({"path": scope})).await;
    assert_eq!(
        ok_get(&app, "/v1/projects").await["projects"][0]["roots"],
        json!([scope])
    );
    let vcs = ok_get(&app, &format!("/v1/vcs?directory={}", enc(&scope))).await;
    assert!(vcs["branch"].is_null() || vcs["branch"] == json!(""));
    assert!(vcs["files"].as_array().is_none_or(Vec::is_empty));

    // Workflows scope forwarding + refusal of removed x-hya-directory header.
    let _ = ok_get(&app, "/v1/workflows").await;
    let field_dir = std::env::temp_dir().join("hya-v1-list-workflows-field");
    let wf_uri = format!(
        "/v1/workflows?directory={}",
        enc(&field_dir.to_string_lossy())
    );
    let _ = ok_get(&app, &wf_uri).await;
    assert_eq!(
        send_raw(
            &app,
            Method::GET,
            &wf_uri,
            &[("x-hya-directory", "/anywhere")],
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        wf_control.seen.lock().unwrap().as_slice(),
        &[None, Some(field_dir)]
    );

    // PTY lifecycle: shells, create, get, connect-token, delete, 404.
    assert!(
        ok_get(&app, "/v1/pty/shells").await["shells"]
            .as_array()
            .is_none_or(|r| r.iter().all(|x| x.is_string()))
    );
    let id =
        id_str(&ok_post(&app, "/v1/pty", json!({"shell": "/bin/sh", "cwd": scope})).await["id"]);
    assert_eq!(
        ok_get(&app, &format!("/v1/pty/{id}")).await["id"],
        json!(id)
    );
    let tok = ok_post(&app, &format!("/v1/pty/{id}/connect-token"), Value::Null).await;
    assert!(tok["token"].as_str().is_some_and(|t| !t.is_empty()));
    assert!(tok["url"].as_str().is_some_and(|u| u.contains("/connect")));
    let _ = ok_del(&app, &format!("/v1/pty/{id}")).await;
    assert_err(
        get_json(&app, &format!("/v1/pty/{id}")).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn pagination_preserves_contents_and_cursors() {
    let app = router(hello_state().await);
    let mut session = String::new();
    for _ in 0..5 {
        session = create_session(&app, &std::env::temp_dir()).await;
    }
    for _ in 0..3 {
        run_prompt(&app, &session, "hello").await;
    }

    for (path, field) in [
        ("/v1/sessions".to_owned(), "sessions"),
        (format!("/v1/sessions/{session}/messages"), "messages"),
    ] {
        let rows = ok_get(&app, &path).await[field]
            .as_array()
            .cloned()
            .unwrap();
        for offset in [0, 2, 4, rows.len()] {
            let cursor = hya_api::cursor::encode_offset(offset as u64);
            let page = ok_get(&app, &format!("{path}?page.limit=2&page.cursor={cursor}")).await;
            let expected: Vec<_> = rows.iter().skip(offset).take(2).cloned().collect();
            assert_eq!(
                page[field],
                if expected.is_empty() {
                    Value::Null
                } else {
                    json!(expected)
                }
            );
            let next = offset + expected.len();
            let more = next < rows.len();
            assert_eq!(page["page"]["hasMore"].as_bool().unwrap_or(false), more);
            assert_eq!(
                page["page"]["nextCursor"].as_str().unwrap_or(""),
                if more {
                    hya_api::cursor::encode_offset(next as u64)
                } else {
                    String::new()
                }
            );
        }
    }

    let path = format!("/v1/sessions/{session}/events");
    let all = ok_get(&app, &format!("{path}?includeRaw=true")).await;
    let raw = all["rawEnvelopes"].as_array().unwrap();
    let seq_of = |line: &Value| {
        serde_json::from_str::<Value>(line.as_str().unwrap()).unwrap()["seq"]
            .as_u64()
            .unwrap()
    };
    for offset in [0, raw.len() / 2, raw.len().saturating_sub(1), raw.len()] {
        let since = if offset == 0 {
            0
        } else {
            seq_of(&raw[offset - 1])
        };
        let page = ok_get(
            &app,
            &format!("{path}?includeRaw=true&limit=1&sinceSeq={since}"),
        )
        .await;
        let expected: Vec<_> = raw.iter().skip(offset).take(1).cloned().collect();
        assert_eq!(
            page["rawEnvelopes"],
            if expected.is_empty() {
                Value::Null
            } else {
                json!(expected)
            }
        );
        let next = expected.first().map_or(since, seq_of);
        let expected_events: Vec<_> = all["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                let s: u64 = e["seq"].as_str().unwrap().parse().unwrap();
                s > since && s <= next
            })
            .cloned()
            .collect();
        assert_eq!(
            page["events"],
            if expected_events.is_empty() {
                Value::Null
            } else {
                json!(expected_events)
            }
        );
        assert_eq!(
            page["nextSeq"].as_str().unwrap().parse::<u64>().unwrap(),
            next
        );
    }
}
