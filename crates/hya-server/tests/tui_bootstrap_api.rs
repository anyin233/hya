//! `/tui/bootstrap` single-RTT startup payload.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::Value;
use tower::ServiceExt as _;

async fn app() -> axum::Router {
    let store = SessionStore::connect_memory().await.unwrap();
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![]));
    let engine = Arc::new(SessionEngine::new(
        store,
        providers,
        tools,
        permission,
        EventBus::default(),
    ));
    let agent = Arc::new(AgentSpec {
        name: hya_proto::AgentName::new("build"),
        model: hya_proto::ModelRef::new("dev/fake"),
        system_prompt: "test".into(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    });
    router(AppState::new(engine, agent))
}

async fn get_json(app: axum::Router, path: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .header(
                    "x-opencode-directory",
                    std::env::temp_dir().to_string_lossy().as_ref(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn tui_bootstrap_returns_required_startup_fields() {
    let (status, body) = get_json(app().await, "/tui/bootstrap").await;
    assert_eq!(status, StatusCode::OK);
    for key in [
        "config",
        "providers",
        "provider_list",
        "capabilities",
        "agents",
        "sessions",
        "commands",
        "lsp",
        "mcp",
        "formatter",
        "session_status",
        "vcs",
        "path",
        "project",
    ] {
        assert!(body.get(key).is_some(), "missing {key}");
    }
    // Slim command entries must not ship full skill templates.
    if let Some(commands) = body.get("commands").and_then(Value::as_array) {
        for command in commands {
            assert!(
                command.get("template").is_none(),
                "bootstrap command entries must omit template bodies"
            );
            assert!(command.get("name").is_some());
        }
    }
}
