#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

fn tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-{label}-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn init_repo() -> PathBuf {
    let repo = tempdir("project-copy-repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

async fn state(workdir: PathBuf) -> AppState {
    let provider = Arc::new(FakeProvider::scripted(vec![]));
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, permission, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir,
            reasoning: None,
        }),
    )
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

#[tokio::test]
async fn opencode_project_copy_creates_and_removes_git_worktree() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let repo = init_repo();
    let app = router(state(repo.clone()).await);
    let copy_parent = tempdir("project-copy-parent");
    let copy_dir = copy_parent.join("copy");
    let uri = "/experimental/project/global/copy";

    let created = request(
        app.clone(),
        "POST",
        uri,
        Some(json!({
            "strategy": "git_worktree",
            "directory": copy_parent,
            "name": "copy",
        })),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let created_body = body_json(created).await;
    assert_eq!(
        created_body["directory"].as_str().unwrap(),
        copy_dir.to_string_lossy()
    );
    assert!(copy_dir.exists());

    std::fs::write(copy_dir.join("dirty.txt"), "dirty\n").unwrap();
    let rejected = request(
        app.clone(),
        "DELETE",
        uri,
        Some(json!({
            "directory": copy_dir,
            "force": false,
        })),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(rejected).await["data"]["forceRequired"], true);

    let removed = request(
        app.clone(),
        "DELETE",
        uri,
        Some(json!({
            "directory": copy_dir,
            "force": true,
        })),
    )
    .await;
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert!(!copy_dir.exists());

    let refresh = request(
        app,
        "POST",
        "/experimental/project/global/copy/refresh",
        None,
    )
    .await;
    assert_eq!(refresh.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn opencode_project_copy_rejects_unknown_strategy() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let repo = init_repo();
    let app = router(state(repo).await);
    let resp = request(
        app,
        "POST",
        "/experimental/project/global/copy",
        Some(json!({
            "strategy": "archive",
            "directory": tempdir("project-copy-unsupported"),
        })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(resp).await["data"]["message"]
            .as_str()
            .unwrap()
            .contains("strategy unavailable")
    );
}

#[tokio::test]
async fn opencode_project_copy_generates_short_names() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let repo = init_repo();
    let app = router(state(repo).await);

    let contextual = request(
        app.clone(),
        "POST",
        "/experimental/project/global/copy/generate-name",
        Some(json!({
            "context": "Fix MCP OAuth callback flow"
        })),
    )
    .await;
    assert_eq!(contextual.status(), StatusCode::OK);
    assert_eq!(body_json(contextual).await["name"], "fix-mcp-oauth");

    let fallback = request(
        app,
        "POST",
        "/experimental/project/global/copy/generate-name",
        Some(json!({})),
    )
    .await;
    assert_eq!(fallback.status(), StatusCode::OK);
    assert!(
        body_json(fallback).await["name"]
            .as_str()
            .is_some_and(|name| !name.is_empty())
    );
}

#[tokio::test]
async fn opencode_project_routes_return_current_directory() {
    let repo = init_repo();
    let app = router(state(repo.clone()).await);
    let repo_text = repo.to_string_lossy().into_owned();

    let current = request(app.clone(), "GET", "/project/current", None).await;
    assert_eq!(current.status(), StatusCode::OK);
    let current_body = body_json(current).await;
    assert_eq!(current_body["id"], "global");
    assert_eq!(current_body["worktree"], repo_text.as_str());
    assert_eq!(current_body["vcs"], "git");
    assert_eq!(current_body["sandboxes"], json!([]));

    let list = request(app.clone(), "GET", "/project", None).await;
    assert_eq!(list.status(), StatusCode::OK);
    assert_eq!(body_json(list).await[0]["worktree"], repo_text.as_str());

    let directories = request(app, "GET", "/project/global/directories", None).await;
    assert_eq!(directories.status(), StatusCode::OK);
    assert_eq!(
        body_json(directories).await,
        json!([{ "directory": repo_text }])
    );
}
