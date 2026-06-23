#![allow(clippy::unwrap_used)]

use std::sync::Arc;

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

const WORKDIR: &str = "/tmp/yaca-opencode-experimental-tool-api";

async fn state() -> AppState {
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
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn tool_ids(body: &Value) -> Vec<&str> {
    body.as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn opencode_experimental_tool_list_filters_patch_tools_by_model() {
    let app = router(state().await);

    let (status, test_body) = get_json(
        app.clone(),
        "/experimental/tool?provider=opencode&model=test",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let test_ids = tool_ids(&test_body);
    assert!(test_ids.contains(&"edit"));
    assert!(test_ids.contains(&"write"));
    assert!(!test_ids.contains(&"apply_patch"));

    let (status, gpt_body) =
        get_json(app, "/experimental/tool?provider=opencode&model=gpt-5").await;
    assert_eq!(status, StatusCode::OK);
    let gpt_ids = tool_ids(&gpt_body);
    assert!(gpt_ids.contains(&"apply_patch"));
    assert!(!gpt_ids.contains(&"edit"));
    assert!(!gpt_ids.contains(&"write"));
}

#[tokio::test]
async fn opencode_experimental_tool_list_filters_websearch_by_provider() {
    let app = router(state().await);

    let (status, opencode_body) = get_json(
        app.clone(),
        "/experimental/tool?provider=opencode&model=test",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let opencode_ids = tool_ids(&opencode_body);
    assert!(opencode_ids.contains(&"websearch"));

    let (status, anthropic_body) = get_json(
        app,
        "/experimental/tool?provider=anthropic&model=claude-sonnet-4-5",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let anthropic_ids = tool_ids(&anthropic_body);
    assert!(!anthropic_ids.contains(&"websearch"));
}
