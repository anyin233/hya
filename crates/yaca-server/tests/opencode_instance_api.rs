#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
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
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
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
