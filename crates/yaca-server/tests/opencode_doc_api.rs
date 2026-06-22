#![allow(clippy::unwrap_used, clippy::expect_used)]

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
            workdir: "/tmp/yaca-opencode-doc-api".into(),
            reasoning: None,
        }),
    )
}

#[tokio::test]
async fn opencode_doc_route_returns_openapi_document() {
    let app = router(state().await);
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/doc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("application/json"))
    );
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert!(body["openapi"].is_string());
    assert!(body["info"].is_object());
    assert!(body["paths"]["/global/health"].is_object());
    assert!(body["paths"]["/session"].is_object());
    assert!(body["paths"]["/provider"].is_object());
    assert!(body["paths"]["/provider/auth"].is_object());
    assert!(body["paths"]["/file"].is_object());
    assert!(body["paths"]["/find"].is_object());
    assert!(body["paths"]["/find/file"].is_object());
    assert!(body["paths"]["/find/symbol"].is_object());
    assert!(body["paths"]["/permission"].is_object());
    assert!(body["paths"]["/api/provider"].is_object());
    assert!(body["paths"]["/api/model"].is_object());
    assert!(body["paths"]["/api/fs/list"].is_object());
    assert!(body["paths"]["/api/fs/find"].is_object());
    assert!(body["paths"]["/api/permission/request"].is_object());
}
