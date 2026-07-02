#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
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

fn tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-server-{label}-{nanos}-{serial}-{}",
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
        "git {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn init_repo() -> PathBuf {
    let repo = tempdir("experimental-worktree-repo");
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_worktree(app: axum::Router, name: &str) -> PathBuf {
    let created = request(
        app,
        "POST",
        "/experimental/worktree",
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    PathBuf::from(
        body_json(created).await["directory"]
            .as_str()
            .expect("worktree directory"),
    )
}

#[tokio::test]
async fn compat_experimental_worktree_create_list_and_remove_use_git_worktrees() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let repo = init_repo();
    let app = router(state(repo).await);

    let directory = create_worktree(app.clone(), "api-dsl").await;
    assert!(directory.exists());

    let listed = request(app.clone(), "GET", "/experimental/worktree", None).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = body_json(listed).await;
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some(directory.to_string_lossy().as_ref()))
    );

    let removed = request(
        app,
        "DELETE",
        "/experimental/worktree",
        Some(json!({ "directory": directory })),
    )
    .await;
    assert_eq!(removed.status(), StatusCode::OK);
    assert_eq!(body_json(removed).await, json!(true));
    assert!(!directory.exists());
}

#[tokio::test]
async fn compat_experimental_worktree_reset_restores_and_cleans_directory() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let repo = init_repo();
    let app = router(state(repo).await);
    let directory = create_worktree(app.clone(), "api-reset").await;

    std::fs::write(directory.join("README.md"), "dirty\n").unwrap();
    std::fs::write(directory.join("scratch.txt"), "scratch\n").unwrap();

    let reset = request(
        app.clone(),
        "POST",
        "/experimental/worktree/reset",
        Some(json!({ "directory": directory })),
    )
    .await;
    assert_eq!(reset.status(), StatusCode::OK);
    assert_eq!(body_json(reset).await, json!(true));
    assert_eq!(
        std::fs::read_to_string(directory.join("README.md")).unwrap(),
        "hello\n"
    );
    assert!(!directory.join("scratch.txt").exists());

    let removed = request(
        app,
        "DELETE",
        "/experimental/worktree",
        Some(json!({ "directory": directory })),
    )
    .await;
    assert_eq!(removed.status(), StatusCode::OK);
}

#[tokio::test]
async fn compat_experimental_worktree_create_rejects_invalid_payload() {
    let app = router(state(tempdir("experimental-worktree-invalid")).await);
    let response = request(
        app,
        "POST",
        "/experimental/worktree",
        Some(json!({ "name": 1 })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
