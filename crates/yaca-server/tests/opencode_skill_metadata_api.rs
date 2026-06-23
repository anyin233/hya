#![allow(clippy::unwrap_used)]

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
        "yaca-server-skill-metadata-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: PathBuf) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, permission, EventBus::default());
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

fn find_named<'a>(items: &'a Value, name: &str) -> &'a Value {
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == name)
        .unwrap_or_else(|| panic!("missing {name}: {items}"))
}

#[tokio::test]
async fn opencode_skill_and_command_routes_include_builtin_customize_skill() {
    // Given: a server with no workspace skills on disk.
    let app = router(state(tempdir()).await);

    // When: the OpenCode skill and command metadata routes are listed.
    let (skill_status, skills) = get_json(app.clone(), "/skill").await;
    let (command_status, commands) = get_json(app, "/command").await;

    // Then: OpenCode's built-in customize-opencode skill is present in both surfaces.
    assert_eq!(skill_status, StatusCode::OK);
    let skill = find_named(&skills, "customize-opencode");
    assert_eq!(skill["location"], "<built-in>");
    assert!(
        skill["description"]
            .as_str()
            .unwrap()
            .starts_with("Use ONLY")
    );
    assert!(
        skill["content"]
            .as_str()
            .unwrap()
            .contains("# Customizing opencode")
    );

    assert_eq!(command_status, StatusCode::OK);
    let command = find_named(&commands, "customize-opencode");
    assert_eq!(command["source"], "skill");
    assert!(
        command["template"]
            .as_str()
            .unwrap()
            .contains("opencode.json")
    );
}
