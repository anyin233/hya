#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef, SessionId};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-session-v2-api";

async fn state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(&bytes).unwrap()
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
    (status, body_json(resp).await)
}

async fn get_json(app: axum::Router, uri: String) -> (StatusCode, Value) {
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
    (status, body_json(resp).await)
}

#[tokio::test]
async fn opencode_v2_session_routes_create_get_and_list_wrapped_data() {
    let app = router(state().await);
    let requested = SessionId::new().to_string();

    let (status, created) = post_json(
        app.clone(),
        "/api/session",
        json!({
            "id": requested,
            "agent": "plan",
            "model": {"providerID": "anthropic", "id": "claude-sonnet"},
            "location": {"directory": WORKDIR}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["data"]["id"], requested);
    assert_eq!(created["data"]["agent"], "plan");
    assert_eq!(created["data"]["model"]["providerID"], "anthropic");
    assert_eq!(created["data"]["model"]["id"], "claude-sonnet");
    assert_eq!(created["data"]["directory"], WORKDIR);

    let (status, existing) = post_json(
        app.clone(),
        "/api/session",
        json!({
            "id": requested,
            "agent": "build",
            "model": {"providerID": "openai", "id": "gpt-5"},
            "location": {"directory": "/tmp/ignored"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(existing["data"]["id"], requested);
    assert_eq!(existing["data"]["agent"], "plan");
    assert_eq!(existing["data"]["model"]["providerID"], "anthropic");

    let (status, got) = get_json(app.clone(), format!("/api/session/{requested}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["data"]["id"], requested);

    let (status, _) = post_json(
        app.clone(),
        "/api/session",
        json!({"location": {"directory": WORKDIR}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, listed) = get_json(app, "/api/session?limit=1".to_string()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["data"].as_array().expect("data").len(), 1);
    assert!(listed["cursor"]["next"].as_str().is_some());
    assert!(listed["cursor"]["previous"].as_str().is_some());
}
