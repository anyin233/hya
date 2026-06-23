#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-{label}-{nanos}-{serial}-{}",
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
    let repo = tempdir("workspace-api-repo");
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
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

#[tokio::test]
async fn opencode_workspace_create_list_and_remove_use_worktree_adapter() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let repo = init_repo();
    let app = router(state(repo).await);

    let created = request(
        app.clone(),
        "POST",
        "/experimental/workspace",
        Some(json!({ "type": "worktree", "branch": null })),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let created = body_json(created).await;
    assert_eq!(created["type"], "worktree");
    assert!(created["id"].as_str().unwrap().starts_with("wrk_"));
    assert_eq!(
        created["name"],
        created["directory"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
    );
    assert!(Path::new(created["directory"].as_str().unwrap()).exists());

    let listed = request(app.clone(), "GET", "/experimental/workspace", None).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = body_json(listed).await;
    assert!(listed.as_array().unwrap().iter().any(|workspace| {
        workspace["id"] == created["id"] && workspace["directory"] == created["directory"]
    }));

    let removed = request(
        app.clone(),
        "DELETE",
        &format!(
            "/experimental/workspace/{}",
            created["id"].as_str().unwrap()
        ),
        None,
    )
    .await;
    assert_eq!(removed.status(), StatusCode::OK);
    let removed = body_json(removed).await;
    assert_eq!(removed["id"], created["id"]);
    assert!(!Path::new(created["directory"].as_str().unwrap()).exists());

    let listed = request(app, "GET", "/experimental/workspace", None).await;
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(body_json(listed).await, json!([]));
}
