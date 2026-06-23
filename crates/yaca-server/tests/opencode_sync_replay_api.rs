#![allow(clippy::unwrap_used)]

use std::sync::Arc;

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
            workdir: "/tmp/yaca-opencode-sync-replay-api".into(),
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
async fn opencode_sync_replay_accepts_non_empty_event_history() {
    let (status, body) = post_json(
        router(state().await),
        "/sync/replay",
        json!({
            "directory": "/tmp/yaca-opencode-sync-replay-api",
            "events": [{
                "id": "evt_00000000000000000000000000",
                "aggregateID": "ses_00000000000000000000000000000000",
                "seq": 0,
                "type": "session.updated",
                "data": {}
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessionID"], "ses_00000000000000000000000000000000");
}

#[tokio::test]
async fn opencode_sync_replay_events_are_returned_by_history() {
    let app = router(state().await);
    let aggregate = "ses_00000000000000000000000000000000";
    let (status, body) = post_json(
        app.clone(),
        "/sync/replay",
        json!({
            "directory": "/tmp/yaca-opencode-sync-replay-api",
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

    let (status, history) = post_json(app.clone(), "/sync/history", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history[0]["aggregate_id"], aggregate);
    assert_eq!(history[0]["type"], "session.updated");
    assert_eq!(history[0]["data"]["title"], "remote");

    let (status, filtered) = post_json(app, "/sync/history", json!({aggregate: 0})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(filtered, json!([]));
}

#[tokio::test]
async fn opencode_sync_history_orders_replayed_events_by_sequence() {
    let app = router(state().await);
    let first = "ses_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let second = "ses_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for (aggregate, seq) in [(first, 1_u64), (second, 2_u64)] {
        let (status, _body) = post_json(
            app.clone(),
            "/sync/replay",
            json!({
                "directory": "/tmp/yaca-opencode-sync-replay-api",
                "events": [{
                    "id": format!("evt_{seq}"),
                    "aggregateID": aggregate,
                    "seq": seq,
                    "type": "session.updated",
                    "data": {}
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, history) = post_json(app, "/sync/history", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history[0]["aggregate_id"], first);
    assert_eq!(history[0]["seq"], 1);
    assert_eq!(history[1]["aggregate_id"], second);
    assert_eq!(history[1]["seq"], 2);
}

#[tokio::test]
async fn opencode_sync_replay_rejects_mixed_aggregates() {
    let (status, _body) = post_json(
        router(state().await),
        "/sync/replay",
        json!({
            "directory": "/tmp/yaca-opencode-sync-replay-api",
            "events": [
                {
                    "id": "evt_00000000000000000000000000",
                    "aggregateID": "ses_00000000000000000000000000000000",
                    "seq": 0,
                    "type": "session.updated",
                    "data": {}
                },
                {
                    "id": "evt_00000000000000000000000001",
                    "aggregateID": "ses_11111111111111111111111111111111",
                    "seq": 1,
                    "type": "session.updated",
                    "data": {}
                }
            ]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn opencode_sync_replay_rejects_non_contiguous_sequences() {
    let (status, _body) = post_json(
        router(state().await),
        "/sync/replay",
        json!({
            "directory": "/tmp/yaca-opencode-sync-replay-api",
            "events": [
                {
                    "id": "evt_00000000000000000000000000",
                    "aggregateID": "ses_00000000000000000000000000000000",
                    "seq": 10,
                    "type": "session.updated",
                    "data": {}
                },
                {
                    "id": "evt_00000000000000000000000001",
                    "aggregateID": "ses_00000000000000000000000000000000",
                    "seq": 12,
                    "type": "session.updated",
                    "data": {}
                }
            ]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}
