#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-compat-legacy-tool-state-api";

async fn state() -> AppState {
    std::fs::create_dir_all(WORKDIR).unwrap();
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, permission, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn request_json(app: axum::Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn legacy_tool_parts_include_compat_state_shape() {
    let app = router(state().await);
    let created = request_json(
        app.clone(),
        "POST",
        "/sessions",
        Some(json!({"agent": "build", "model": "fake", "workdir": WORKDIR})),
    )
    .await;
    let session = created["session"].as_str().unwrap();

    let message = request_json(
        app,
        "POST",
        &format!("/session/{session}/shell"),
        Some(json!({"agent": "build", "command": "printf hello"})),
    )
    .await;
    let state = &message["parts"][0]["state"];

    assert_eq!(state["status"], "completed");
    assert_eq!(state["input"]["command"], "printf hello");
    assert_eq!(state["output"], "hello");
    assert_eq!(state["title"], "printf hello");
    assert_eq!(state["metadata"]["exit"], 0);
    assert_eq!(state["metadata"]["output"], "hello");
    assert!(state["time"]["start"].as_u64().is_some());
    assert!(state["time"]["end"].as_u64() >= state["time"]["start"].as_u64());
}
