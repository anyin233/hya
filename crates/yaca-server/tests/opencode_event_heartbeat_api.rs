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

const WORKDIR: &str = "/tmp/yaca-opencode-event-heartbeat-api";

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

async fn event_stream(uri: &str) -> axum::body::BodyDataStream {
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
    resp.into_body().into_data_stream()
}

#[tokio::test]
async fn opencode_event_routes_stream_heartbeat_events() {
    assert_event_heartbeat("/event").await;
    assert_event_heartbeat(
        "/api/event?location%5Bdirectory%5D=/tmp/yaca-opencode-event-heartbeat-api",
    )
    .await;
}

#[tokio::test]
async fn opencode_global_event_route_streams_heartbeat_events() {
    let mut stream = event_stream("/global/event").await;
    let connected = read_sse_json(&mut stream).await;
    assert_eq!(connected["payload"]["type"], "server.connected");

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    let heartbeat = read_sse_json(&mut stream).await;
    tokio::time::resume();
    assert_eq!(heartbeat["payload"]["type"], "server.heartbeat");
    assert_eq!(heartbeat["payload"]["properties"], json!({}));
}

async fn assert_event_heartbeat(uri: &str) {
    let mut stream = event_stream(uri).await;
    let connected = read_sse_json(&mut stream).await;
    assert_eq!(connected["type"], "server.connected");

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::task::yield_now().await;
    let heartbeat = read_sse_json(&mut stream).await;
    tokio::time::resume();
    assert_eq!(heartbeat["type"], "server.heartbeat");
    assert_eq!(heartbeat["properties"], json!({}));
    assert!(heartbeat.get("location").is_none());
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
