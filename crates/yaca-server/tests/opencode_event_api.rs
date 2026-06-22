#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use serde_json::json;
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-event-api";

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

#[tokio::test]
async fn opencode_v2_event_route_streams_connected_event() {
    assert_event_stream("/api/event").await;
}

#[tokio::test]
async fn opencode_legacy_event_route_streams_connected_event() {
    assert_event_stream("/event").await;
}

#[tokio::test]
async fn opencode_global_event_route_streams_connected_event() {
    assert_event_stream("/global/event").await;
}

#[tokio::test]
async fn opencode_v2_event_route_streams_session_created_location() {
    let app = router(state().await);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    let connected = read_sse_json(&mut stream).await;
    assert_eq!(connected["type"], "server.connected");

    let directory = "/tmp/yaca-opencode-event-api-scoped";
    let created = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"location": {"directory": directory}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let event = read_sse_json(&mut stream).await;
    assert_eq!(event["type"], "session.created");
    assert_eq!(event["location"]["directory"], directory);
    assert!(event["data"]["sessionID"].as_str().is_some());
}

async fn assert_event_stream(uri: &str) {
    let app = router(state().await);
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
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let mut stream = resp.into_body().into_data_stream();
    let event = read_sse_json(&mut stream).await;
    assert_eq!(event["type"], "server.connected");
    assert!(event.get("location").is_none());
}

async fn read_sse_json(stream: &mut axum::body::BodyDataStream) -> serde_json::Value {
    let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("event")
        .expect("body chunk")
        .expect("valid chunk");
    let frame = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(frame.contains("data:"));
    let data = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("data line");
    serde_json::from_str(data).unwrap()
}
