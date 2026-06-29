#![allow(clippy::unwrap_used)]

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
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-sync-history-api";

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

async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = app
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
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

async fn create_session(app: axum::Router) -> String {
    let (status, body) = post_json(
        app,
        "/sessions",
        json!({"agent": "build", "model": "fake", "workdir": WORKDIR}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["session"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn opencode_sync_history_returns_events_after_known_sequences() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    let (status, history) = post_json(app.clone(), "/sync/history", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let events = history.as_array().unwrap();
    let event = events
        .iter()
        .find(|event| event["aggregate_id"] == session)
        .unwrap();
    assert!(event["id"].as_str().unwrap().starts_with("evt_"));
    assert_eq!(event["type"], "session_created");
    let seq = event["seq"].as_u64().unwrap();

    let (status, caught_up) = post_json(app, "/sync/history", json!({ session: seq })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(caught_up, json!([]));
}
