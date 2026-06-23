#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

fn db_path() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!(
            "yaca-opencode-sync-persistence-{nanos}-{}.db",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

async fn state(path: &str) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect(path).await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: "/tmp/yaca-opencode-sync-persistence-api".into(),
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

#[tokio::test]
async fn opencode_sync_replay_persists_history_across_state() {
    let path = db_path();
    let aggregate = "ses_00000000000000000000000000000000";
    let (status, body) = post_json(
        router(state(&path).await),
        "/sync/replay",
        json!({
            "directory": "/tmp/yaca-opencode-sync-persistence-api",
            "events": [{
                "id": "evt_00000000000000000000000000",
                "aggregateID": aggregate,
                "seq": 0,
                "type": "session.updated",
                "data": {"title": "remote"}
            }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessionID"], aggregate);

    let (status, history) = post_json(router(state(&path).await), "/sync/history", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history[0]["aggregate_id"], aggregate);
    assert_eq!(history[0]["data"]["title"], "remote");
}
