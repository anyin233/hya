//! `hya serve` has no working directory (ADR-0024): an rpc that needs a
//! directory takes it from the request (`x-hya-directory` / `directory`) or
//! from the session; without either it fails with `invalid_argument`.
//! Catalog rpcs that only *prefer* a directory answer from global sources.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A process agent whose workdir points nowhere: nothing may read it.
async fn state() -> AppState {
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
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: PathBuf::from("/nonexistent/hya-process-agent-workdir"),
            reasoning: None,
        }),
    )
}

async fn send(
    app: axum::Router,
    method: Method,
    uri: &str,
    directory: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(directory) = directory {
        builder = builder.header("x-hya-directory", directory);
    }
    let resp = app.oneshot(builder.body(body).unwrap()).await.unwrap();
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
        "hya-v1-scope-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

/// Every rpc that works on a directory, with and without a body.
fn scoped_calls() -> Vec<(Method, &'static str, Value)> {
    vec![
        (Method::GET, "/v1/fs/read?path=notes.txt", Value::Null),
        (Method::GET, "/v1/fs/list", Value::Null),
        (Method::GET, "/v1/fs/find?pattern=*", Value::Null),
        (Method::GET, "/v1/fs/search?query=x", Value::Null),
        (Method::GET, "/v1/fs/symbols?query=x", Value::Null),
        (Method::GET, "/v1/vcs", Value::Null),
        (Method::GET, "/v1/vcs/diff", Value::Null),
        (Method::POST, "/v1/vcs/apply", json!({"patch": ""})),
        (Method::GET, "/v1/worktrees", Value::Null),
        (Method::POST, "/v1/worktrees", json!({})),
        (Method::DELETE, "/v1/worktrees/some-id", Value::Null),
        (Method::POST, "/v1/worktrees/some-id/reset", Value::Null),
        (Method::POST, "/v1/pty", json!({"shell": "/bin/sh"})),
    ]
}

#[tokio::test]
async fn scoped_rpcs_without_a_directory_are_invalid_argument() {
    let app = router(state().await);
    for (method, uri, body) in scoped_calls() {
        let (status, reply) = send(app.clone(), method.clone(), uri, None, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {uri}: {reply}");
        assert_eq!(
            reply["error"]["code"],
            json!("invalid_argument"),
            "{method} {uri}: {reply}"
        );
        let message = reply["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("directory scope"),
            "{method} {uri}: {message}"
        );
    }
}

#[tokio::test]
async fn a_relative_directory_scope_is_invalid_argument() {
    let app = router(state().await);
    for (method, uri, body) in scoped_calls() {
        let (status, reply) = send(app.clone(), method.clone(), uri, Some("."), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {uri}: {reply}");
        assert_eq!(reply["error"]["code"], json!("invalid_argument"));
    }
    let (status, reply) = send(
        app,
        Method::GET,
        "/v1/agents",
        Some("relative/dir"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    assert_eq!(reply["error"]["code"], json!("invalid_argument"));
}

#[tokio::test]
async fn scoped_rpcs_with_a_directory_work_on_it() {
    let app = router(state().await);
    let dir = scratch("with-dir");
    std::fs::write(dir.join("notes.txt"), "hello").unwrap();
    let scope = dir.to_string_lossy().into_owned();

    let (status, reply) = send(
        app.clone(),
        Method::GET,
        "/v1/fs/read?path=notes.txt",
        Some(&scope),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (status, reply) = send(
        app.clone(),
        Method::GET,
        "/v1/fs/list",
        Some(&scope),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(
        reply["entries"].to_string().contains("notes.txt"),
        "{reply}"
    );
    let (status, reply) = send(app, Method::GET, "/v1/vcs", Some(&scope), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}

#[tokio::test]
async fn pty_create_takes_its_cwd_from_the_request_or_the_scope() {
    let app = router(state().await);
    let dir = scratch("pty");
    let scope = dir.to_string_lossy().into_owned();
    let (status, reply) = send(
        app.clone(),
        Method::POST,
        "/v1/pty",
        Some(&scope),
        json!({"shell": "/bin/sh"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["cwd"], json!(scope), "{reply}");
    let id = reply["id"].as_str().unwrap().to_owned();
    let _ = send(
        app.clone(),
        Method::DELETE,
        &format!("/v1/pty/{id}"),
        None,
        Value::Null,
    )
    .await;

    let (status, reply) = send(
        app.clone(),
        Method::POST,
        "/v1/pty",
        None,
        json!({"shell": "/bin/sh", "cwd": scope}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let id = reply["id"].as_str().unwrap().to_owned();
    let _ = send(
        app,
        Method::DELETE,
        &format!("/v1/pty/{id}"),
        None,
        Value::Null,
    )
    .await;
}

#[tokio::test]
async fn catalog_rpcs_without_a_directory_answer_from_global_sources() {
    let app = router(state().await);
    for uri in [
        "/v1/agents",
        "/v1/commands",
        "/v1/skills",
        "/v1/bootstrap",
        "/v1/agent-models",
        "/v1/models",
        "/v1/providers",
        "/v1/tools",
        "/v1/permissions/rules",
        "/v1/config",
    ] {
        let (status, reply) = send(app.clone(), Method::GET, uri, None, Value::Null).await;
        assert_ne!(status, StatusCode::BAD_REQUEST, "{uri}: {reply}");
        assert_ne!(status, StatusCode::INTERNAL_SERVER_ERROR, "{uri}: {reply}");
    }
    let (status, agents) = send(app.clone(), Method::GET, "/v1/agents", None, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{agents}");
    assert!(agents["agents"].to_string().contains("build"), "{agents}");
    let (status, commands) = send(app, Method::GET, "/v1/commands", None, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{commands}");
    // Builtin commands are global; `${path}` has no directory to name.
    assert!(
        commands["commands"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "{commands}"
    );
}

#[tokio::test]
async fn project_commands_come_only_from_the_named_directory() {
    let app = router(state().await);
    let dir = scratch("commands");
    std::fs::create_dir_all(dir.join(".hya/commands")).unwrap();
    std::fs::write(dir.join(".hya/commands/scoped-only.md"), "scoped body").unwrap();
    let scope = dir.to_string_lossy().into_owned();

    let (status, reply) = send(
        app.clone(),
        Method::GET,
        "/v1/commands",
        Some(&scope),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(reply.to_string().contains("scoped-only"), "{reply}");
    let (status, reply) = send(app, Method::GET, "/v1/commands", None, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(!reply.to_string().contains("scoped-only"), "{reply}");
}

#[tokio::test]
async fn location_reports_the_request_scope_not_a_process_directory() {
    let app = router(state().await);
    let (status, reply) = send(app.clone(), Method::GET, "/v1/location", None, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(
        reply["directory"].as_str().is_none_or(str::is_empty),
        "{reply}"
    );
    let dir = scratch("location");
    let scope = dir.to_string_lossy().into_owned();
    let (status, reply) = send(app, Method::GET, "/v1/location", Some(&scope), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["directory"], json!(scope), "{reply}");
}

#[tokio::test]
async fn fork_keeps_the_source_workdir_not_the_process_agent_workdir() {
    let app = router(state().await);
    let dir = scratch("fork");
    let workdir = dir.to_string_lossy().into_owned();
    let (status, created) = send(
        app.clone(),
        Method::POST,
        "/v1/sessions",
        None,
        json!({"agent": "build", "model": "fake", "workdir": workdir}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let source = created["session"]["id"].as_str().unwrap().to_owned();
    let (status, forked) = send(
        app,
        Method::POST,
        &format!("/v1/sessions/{source}/fork"),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{forked}");
    assert_eq!(forked["session"]["workdir"], json!(workdir), "{forked}");
}

#[tokio::test]
async fn grpc_scoped_rpc_without_a_directory_is_invalid_argument() {
    use hya_api::v1::files_server::Files as _;
    let grpc = V1Grpc::new(state().await);
    let error = grpc
        .read_file(tonic::Request::new(pb::ReadFileRequest {
            path: "notes.txt".to_owned(),
            ..Default::default()
        }))
        .await
        .expect_err("no directory scope");
    assert_eq!(error.code(), tonic::Code::InvalidArgument, "{error:?}");
    assert!(error.message().contains("directory scope"), "{error:?}");
}
