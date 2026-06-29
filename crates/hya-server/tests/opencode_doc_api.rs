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
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::Value;
use tower::ServiceExt;

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
            workdir: "/tmp/hya-opencode-doc-api".into(),
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
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, body)
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
    assert!(body["paths"]["/config"]["get"].is_object());
    assert!(body["paths"]["/config/providers"]["get"].is_object());
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
    assert!(body["paths"]["/api/fs/read/*"].is_object());
    assert!(body["paths"]["/api/fs/read/{path}"].is_null());
    assert!(body["paths"]["/api/fs/list"].is_object());
    assert!(body["paths"]["/api/fs/find"].is_object());
    assert!(body["paths"]["/api/permission/request"].is_object());
    assert!(body["paths"]["/session/{sessionID}/share"]["delete"].is_object());
    assert!(body["paths"]["/experimental/workspace"]["get"].is_object());
    assert!(body["paths"]["/experimental/workspace"]["post"].is_object());
    assert!(body["paths"]["/experimental/workspace/{id}"]["delete"].is_object());
    assert!(body["paths"]["/experimental/control-plane/move-session"]["post"].is_object());
    assert!(body["paths"]["/experimental/session/{sessionID}/background"].is_object());
    assert!(body["paths"]["/api/session/{sessionID}/permission/{requestID}/reply"].is_object());
    assert!(body["paths"]["/api/session/{sessionID}/message/{messageID}"]["get"].is_object());
    assert!(body["paths"]["/api/session/{sessionID}/question/{requestID}/reject"].is_object());
    assert!(body["paths"]["/api/integration/{integrationID}/connect/key"].is_object());
    assert!(body["paths"]["/api/integration/attempt/{attemptID}"].is_object());
    assert!(body["paths"]["/api/credential/{credentialID}"]["delete"].is_object());
}

#[tokio::test]
async fn opencode_openapi_json_route_returns_openapi_document() {
    let app = router(state().await);
    let (status, body) = get_json(app, "/openapi.json").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["openapi"], "3.1.0");
    assert!(body["paths"]["/api/session"].is_object());
}
