#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-server-location-test-{nanos}-{serial}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn init_repo() -> PathBuf {
    let repo = tempdir();
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

async fn state(workdir: impl Into<std::path::PathBuf>) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("openai/gpt-5"),
            system_prompt: "x".to_string(),
            workdir: workdir.into(),
            reasoning: None,
        }),
    )
}

async fn get_json(app: axum::Router, uri: String, headers: &[(&str, &str)]) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let resp = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn opencode_location_query_and_headers_override_default_workdir() {
    let workdir = tempdir();
    let scoped = workdir.join("scoped dir");
    std::fs::create_dir_all(&scoped).unwrap();
    let scoped = std::fs::canonicalize(scoped).unwrap();
    let scoped_text = scoped.to_string_lossy();
    let encoded_scoped = scoped_text.replace(' ', "%20");
    let app = router(state(workdir).await);

    let (status, location) = get_json(
        app.clone(),
        format!("/api/location?location%5Bdirectory%5D={encoded_scoped}"),
        &[("x-opencode-workspace", "wrk_query")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(location["directory"], scoped_text.as_ref());
    assert_eq!(location["workspaceID"], "wrk_query");
    assert_eq!(location["project"]["directory"], scoped_text.as_ref());

    let (status, agents) = get_json(
        app,
        "/api/agent".to_string(),
        &[
            ("x-opencode-directory", &encoded_scoped),
            ("x-opencode-workspace", "wrk_header"),
            (header::ACCEPT.as_str(), "application/json"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agents["location"]["directory"], scoped_text.as_ref());
    assert_eq!(agents["location"]["workspaceID"], "wrk_header");
    assert_eq!(agents["data"][0]["id"], "build");
}

#[tokio::test]
async fn opencode_location_accepts_workspace_routing_query_names() {
    let workdir = tempdir();
    let scoped = workdir.join("workspace routed");
    std::fs::create_dir_all(&scoped).unwrap();
    let scoped = std::fs::canonicalize(scoped).unwrap();
    let scoped_text = scoped.to_string_lossy();
    let encoded_scoped = scoped_text.replace(' ', "%20");
    let app = router(state(workdir).await);

    let (status, location) = get_json(
        app,
        format!("/api/location?directory={encoded_scoped}&workspace=wrk_direct"),
        &[],
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(location["directory"], scoped_text.as_ref());
    assert_eq!(location["workspaceID"], "wrk_direct");
}

#[tokio::test]
async fn opencode_location_workspace_query_routes_to_worktree_directory() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let repo = init_repo();
    let app = router(state(repo).await);
    let (created_status, created) = post_json(
        app.clone(),
        "/experimental/workspace",
        json!({ "type": "worktree", "branch": null }),
    )
    .await;
    assert_eq!(created_status, StatusCode::OK);
    let workspace_id = created["id"].as_str().unwrap();
    let directory = created["directory"].as_str().unwrap();

    let (status, location) = get_json(
        app.clone(),
        format!("/api/location?workspace={workspace_id}"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(location["directory"], directory);
    assert_eq!(location["workspaceID"], workspace_id);

    let (status, agents) = get_json(
        app,
        format!("/api/agent?workspace={workspace_id}"),
        &[(header::ACCEPT.as_str(), "application/json")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agents["location"]["directory"], directory);
}
