#![allow(clippy::unwrap_used)]

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

const WORKDIR: &str = "/tmp/hya-compat-workspace-warp-api";
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

async fn state() -> AppState {
    state_for(PathBuf::from(WORKDIR)).await
}

async fn state_for(workdir: PathBuf) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
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
    let repo = tempdir("workspace-warp-repo");
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "init"]);
    repo
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

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_session(app: axum::Router) -> String {
    create_session_in(app, WORKDIR).await
}

async fn create_session_in(app: axum::Router, workdir: impl AsRef<Path>) -> String {
    let response = request(
        app,
        "POST",
        "/sessions",
        Some(json!({
            "agent": "build",
            "model": "fake",
            "workdir": workdir.as_ref()
        })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["session"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn compat_workspace_warp_moves_session_to_worktree_workspace() {
    if !git_available() {
        eprintln!("skipping: git is not available");
        return;
    }
    let repo = init_repo();
    let app = router(state_for(repo.clone()).await);
    let created = request(
        app.clone(),
        "POST",
        "/experimental/workspace",
        Some(json!({ "type": "worktree", "branch": null })),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    let workspace = body_json(created).await;
    let directory = workspace["directory"].as_str().unwrap().to_string();
    let session = create_session_in(app.clone(), &repo).await;

    let warped = request(
        app.clone(),
        "POST",
        "/experimental/workspace/warp",
        Some(json!({
            "id": workspace["id"].as_str().unwrap(),
            "sessionID": session,
            "copyChanges": false
        })),
    )
    .await;

    assert_eq!(warped.status(), StatusCode::NO_CONTENT);
    let loaded = request(app, "GET", &format!("/session/{session}"), None).await;
    assert_eq!(loaded.status(), StatusCode::OK);
    assert_eq!(body_json(loaded).await["directory"], directory);
}

#[tokio::test]
async fn compat_workspace_warp_detaches_existing_session_to_local_project() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    let response = request(
        app,
        "POST",
        "/experimental/workspace/warp",
        Some(json!({
            "id": null,
            "sessionID": session,
            "copyChanges": false
        })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn compat_workspace_warp_missing_workspace_returns_not_found() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    let response = request(
        app,
        "POST",
        "/experimental/workspace/warp",
        Some(json!({
            "id": "wrk_missing",
            "sessionID": session,
            "copyChanges": false
        })),
    )
    .await;
    let status = response.status();
    let body = body_json(response).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["name"], "NotFoundError");
    assert_eq!(body["data"]["message"], "Workspace not found: wrk_missing");
}
