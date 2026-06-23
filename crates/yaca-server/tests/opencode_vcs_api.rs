#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-vcs-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: PathBuf) -> AppState {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake-model"),
            system_prompt: "system prompt".to_string(),
            workdir,
            reasoning: None,
        }),
    )
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn git(workdir: &PathBuf, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo_with_head(workdir: &PathBuf) {
    git(workdir, &["init"]);
    git(workdir, &["config", "user.email", "test@example.com"]);
    git(workdir, &["config", "user.name", "Test User"]);
    std::fs::write(workdir.join("tracked.txt"), "old\n").unwrap();
    git(workdir, &["add", "tracked.txt"]);
    git(workdir, &["commit", "-m", "initial"]);
}

fn init_branch_repo(workdir: &PathBuf) {
    init_repo_with_head(workdir);
    git(workdir, &["branch", "-M", "main"]);
    git(workdir, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        workdir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(workdir, &["checkout", "-b", "feature"]);
    std::fs::write(workdir.join("tracked.txt"), "new\n").unwrap();
    std::fs::write(workdir.join("branch-only.txt"), "fresh\n").unwrap();
}

#[tokio::test]
async fn opencode_vcs_branch_diff_includes_untracked_files() {
    let workdir = tempdir();
    init_branch_repo(&workdir);
    let app = router(state(workdir).await);

    let (status, diff) = get_json(app, "/vcs/diff?mode=branch&context=1").await;
    assert_eq!(status, StatusCode::OK);
    let items = diff.as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|item| item["file"] == "tracked.txt" && item["status"] == "modified")
    );
    assert!(
        items
            .iter()
            .any(|item| item["file"] == "branch-only.txt" && item["status"] == "added")
    );
}

#[tokio::test]
async fn opencode_vcs_branch_diff_uses_merge_base_for_committed_changes() {
    let workdir = tempdir();
    init_branch_repo(&workdir);
    git(&workdir, &["add", "."]);
    git(&workdir, &["commit", "-m", "feature changes"]);
    let app = router(state(workdir).await);

    let (status, diff) = get_json(app, "/vcs/diff?mode=branch&context=1").await;
    assert_eq!(status, StatusCode::OK);
    let item = diff
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["file"] == "branch-only.txt")
        .unwrap();
    assert_eq!(item["status"], "added");
    assert_eq!(item["additions"], 1);
    assert!(item["patch"].as_str().unwrap().contains("+fresh"));
}

#[tokio::test]
async fn opencode_vcs_diff_caps_oversized_untracked_patch() {
    let workdir = tempdir();
    init_repo_with_head(&workdir);
    std::fs::write(workdir.join("big.txt"), "x".repeat(10_000_001)).unwrap();
    let app = router(state(workdir).await);

    let (status, diff) = get_json(app, "/vcs/diff?mode=git&context=1").await;
    assert_eq!(status, StatusCode::OK);
    let item = diff
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["file"] == "big.txt")
        .unwrap();
    let patch = item["patch"].as_str().unwrap();
    assert!(patch.len() < 1024, "patch was {} bytes", patch.len());
    assert_eq!(
        patch,
        "Index: big.txt\n===================================================================\n--- big.txt\t\n+++ big.txt\t\n"
    );
    assert!(!patch.contains("xxxxxxxxxxxxxxxx"));
}

#[tokio::test]
async fn opencode_vcs_diff_caps_total_patch_bytes() {
    let workdir = tempdir();
    init_repo_with_head(&workdir);
    std::fs::write(workdir.join("a-large.txt"), "a".repeat(5_100_000)).unwrap();
    std::fs::write(workdir.join("b-large.txt"), "b".repeat(5_100_000)).unwrap();
    let app = router(state(workdir).await);

    let (status, diff) = get_json(app, "/vcs/diff?mode=git&context=1").await;
    assert_eq!(status, StatusCode::OK);
    let items = diff.as_array().unwrap();
    let first = items
        .iter()
        .find(|item| item["file"] == "a-large.txt")
        .unwrap()["patch"]
        .as_str()
        .unwrap();
    let second = items
        .iter()
        .find(|item| item["file"] == "b-large.txt")
        .unwrap()["patch"]
        .as_str()
        .unwrap();
    assert!(
        first.len() > 5_000_000,
        "first patch was {} bytes",
        first.len()
    );
    assert!(
        second.len() < 1024,
        "second patch was {} bytes",
        second.len()
    );
    assert!(second.contains("--- b-large.txt"));
    assert!(!second.contains("bbbbbbbbbbbbbbbb"));
}
