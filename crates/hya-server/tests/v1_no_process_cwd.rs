//! The server process's working directory is never a scope (ADR-0024).
//!
//! One test per binary: it moves the process cwd into a directory full of
//! project sources and checks that nothing reads them — not an unscoped
//! catalog listing, and not a session whose workdir is elsewhere.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn send(app: axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
}

fn scratch(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-v1-nocwd-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join(".hya/commands")).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

#[tokio::test]
async fn nothing_reads_the_server_process_working_directory() {
    // The process cwd holds a command named like the session's one.
    let cwd = scratch("cwd");
    std::fs::write(cwd.join(".hya/commands/greet.md"), "from the process cwd").unwrap();
    std::fs::write(cwd.join(".hya/commands/cwd-only.md"), "cwd only").unwrap();
    let workdir = scratch("session");
    std::fs::write(
        workdir.join(".hya/commands/greet.md"),
        "from the session workdir",
    )
    .unwrap();
    std::env::set_current_dir(&cwd).unwrap();

    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("ok".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    let app = router(AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: PathBuf::from("."),
            reasoning: None,
        }),
    ));

    // An unscoped listing is global: the cwd's project commands are absent.
    let (status, commands) = send(app.clone(), Method::GET, "/v1/commands", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{commands}");
    assert!(!commands.to_string().contains("cwd-only"), "{commands}");
    let (status, boot) = send(app.clone(), Method::GET, "/v1/bootstrap", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{boot}");
    assert!(!boot.to_string().contains("cwd-only"), "{boot}");

    // A session-scoped rpc expands commands from the session's workdir.
    let (status, created) = send(
        app.clone(),
        Method::POST,
        "/v1/sessions",
        json!({"agent": "build", "model": "fake", "workdir": workdir.to_string_lossy()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let session = created["session"]["id"].as_str().unwrap().to_owned();
    let (status, turn) = send(
        app.clone(),
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({"command": {"command": "greet", "arguments": ""}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{turn}");
    let mut text = String::new();
    for _ in 0..100 {
        let (status, messages) = send(
            app.clone(),
            Method::GET,
            &format!("/v1/sessions/{session}/messages"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{messages}");
        text = messages.to_string();
        if text.contains("from the") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(text.contains("from the session workdir"), "{text}");
    assert!(!text.contains("from the process cwd"), "{text}");
}
