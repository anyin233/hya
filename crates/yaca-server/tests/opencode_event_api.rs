#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
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
    let app = router(state().await);
    let resp = app
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
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let mut stream = resp.into_body().into_data_stream();
    let chunk = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("first event")
        .expect("body chunk")
        .expect("valid chunk");
    let frame = String::from_utf8(chunk.to_vec()).unwrap();
    assert!(frame.contains("data:"));
    assert!(frame.contains("\"type\":\"server.connected\""));
    assert!(frame.contains("\"directory\":\"/tmp/yaca-opencode-event-api\""));
}
