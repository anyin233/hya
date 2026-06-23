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
use yaca_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-instance-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join(".yaca/skills/demo")).unwrap();
    std::fs::write(
        dir.join(".yaca/skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\n---\nUse this skill.\n",
    )
    .unwrap();
    dir
}

async fn state(workdir: PathBuf) -> AppState {
    state_with_rules(workdir, PermissionRules::default()).await
}

async fn state_with_rules(workdir: PathBuf, rules: PermissionRules) -> AppState {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(rules);
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
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            panic!(
                "non-json response {status}: {} ({err})",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, body)
}

async fn get_text(app: axum::Router, uri: &str) -> (StatusCode, String) {
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
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
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

fn init_git_repo(workdir: &PathBuf) {
    git(workdir, &["init"]);
    git(workdir, &["config", "user.email", "test@example.com"]);
    git(workdir, &["config", "user.name", "Test User"]);
    std::fs::write(workdir.join("tracked.txt"), "old\n").unwrap();
    git(workdir, &["add", "tracked.txt"]);
    git(workdir, &["commit", "-m", "initial"]);
    std::fs::write(workdir.join("tracked.txt"), "new\nextra\n").unwrap();
    std::fs::write(workdir.join("untracked.txt"), "fresh\n").unwrap();
}

#[tokio::test]
async fn opencode_instance_routes_return_metadata() {
    let workdir = tempdir();
    let app = router(state(workdir.clone()).await);
    let workdir = std::fs::canonicalize(workdir).unwrap();

    let (status, paths) = get_json(app.clone(), "/path").await;
    assert_eq!(status, StatusCode::OK);
    let workdir = workdir.to_string_lossy();
    assert_eq!(paths["directory"], workdir.as_ref());
    assert_eq!(paths["worktree"], workdir.as_ref());

    let (status, agents) = get_json(app.clone(), "/agent").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agents[0]["name"], "build");
    assert_eq!(agents[0]["mode"], "primary");
    assert_eq!(agents[0]["model"]["modelID"], "fake-model");

    let (status, commands) = get_json(app.clone(), "/command").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        commands
            .as_array()
            .unwrap()
            .iter()
            .any(|cmd| cmd["name"] == "help")
    );

    let (status, skills) = get_json(app.clone(), "/skill").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(skills[0]["name"], "demo");
    assert_eq!(skills[0]["description"], "Demo skill");
    assert!(
        skills[0]["content"]
            .as_str()
            .unwrap()
            .contains("Use this skill.")
    );

    let (status, lsp) = get_json(app.clone(), "/lsp").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lsp, serde_json::json!([]));

    let (status, formatter) = get_json(app.clone(), "/formatter").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(formatter, serde_json::json!([]));

    let (status, vcs) = get_json(app, "/vcs").await;
    assert_eq!(status, StatusCode::OK);
    assert!(vcs.is_object());
}

#[tokio::test]
async fn opencode_agent_routes_expose_permission_rules() {
    let app = router(
        state_with_rules(
            tempdir(),
            PermissionRules::new(vec![
                Rule::new(Action::Read, "*", Mode::Allow),
                Rule::new(Action::Bash, "git *", Mode::Ask),
                Rule::new(Action::ExternalDirectory, "/tmp/*", Mode::Deny),
            ]),
        )
        .await,
    );

    let expected = serde_json::json!([
        {"permission": "read", "pattern": "*", "action": "allow"},
        {"permission": "bash", "pattern": "git *", "action": "ask"},
        {"permission": "external_directory", "pattern": "/tmp/*", "action": "deny"},
    ]);

    let (status, agents) = get_json(app.clone(), "/agent").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agents[0]["permission"], expected);

    let (status, agents) = get_json(app, "/api/agent").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agents["data"][0]["permissions"], expected);
}

#[tokio::test]
async fn opencode_vcs_routes_return_status_diff_and_apply_patch() {
    let workdir = tempdir();
    init_git_repo(&workdir);
    std::fs::create_dir_all(workdir.join("nested")).unwrap();
    std::fs::write(workdir.join("nested/new.txt"), "deep\n").unwrap();
    std::fs::write(workdir.join("nested/new\nline.txt"), "odd\n").unwrap();
    let app = router(state(workdir.clone()).await);

    let (status, status_body) = get_json(app.clone(), "/vcs/status").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        status_body
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["file"] == "tracked.txt" && item["status"] == "modified")
    );
    assert!(
        status_body
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["file"] == "nested/new.txt" && item["status"] == "added")
    );
    assert!(
        status_body
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["file"] == "nested/new\nline.txt" && item["status"] == "added")
    );

    let (status, diff) = get_json(app.clone(), "/vcs/diff?mode=git&context=1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(diff.as_array().unwrap().iter().any(
        |item| item["file"] == "tracked.txt" && item["patch"].as_str().unwrap().contains("new")
    ));
    assert!(
        diff.as_array()
            .unwrap()
            .iter()
            .any(|item| item["file"] == "untracked.txt" && item["status"] == "added")
    );
    assert!(
        diff.as_array()
            .unwrap()
            .iter()
            .any(|item| item["file"] == "nested/new.txt" && item["status"] == "added")
    );
    assert!(
        diff.as_array()
            .unwrap()
            .iter()
            .any(|item| item["file"] == "nested/new\nline.txt" && item["status"] == "added")
    );

    let (status, raw) = get_text(app.clone(), "/vcs/diff/raw").await;
    assert_eq!(status, StatusCode::OK);
    assert!(raw.contains("diff --git"));
    assert!(raw.contains("tracked.txt"));

    let patch = "diff --git a/applied.txt b/applied.txt\nnew file mode 100644\nindex 0000000..257cc56\n--- /dev/null\n+++ b/applied.txt\n@@ -0,0 +1 @@\n+applied\n";
    let (status, applied) =
        post_json(app, "/vcs/apply", serde_json::json!({ "patch": patch })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(applied["applied"], true);
    assert_eq!(
        std::fs::read_to_string(workdir.join("applied.txt")).unwrap(),
        "applied\n"
    );
}
