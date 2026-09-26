//! Shutdown ends live streams (ADR-0023): the backend is a daemon that
//! outlives its clients, so `hya serve stop` must finish while TUIs still
//! hold event streams open. `StreamShutdown::close` ends every open stream
//! (and every later one) and makes health answer `unavailable`, so attached
//! clients notice at once and reconnect to the next server.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn base_state() -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    )
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn open_stream(app: &axum::Router, uri: &str) -> axum::body::BodyDataStream {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "{uri}");
    resp.into_body().into_data_stream()
}

/// Read until the body ends; `false` when it is still open after `wait`.
async fn ends_within(stream: &mut axum::body::BodyDataStream, wait: Duration) -> bool {
    tokio::time::timeout(wait, async {
        while let Some(chunk) = stream.next().await {
            if chunk.is_err() {
                break;
            }
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn closing_ends_open_streams_and_health_reports_unavailable() {
    let state = base_state().await;
    let shutdown = state.streams();
    let app = router(state);
    let (status, health) = get(&app, "/v1/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["ok"], json!(true));

    let mut global = open_stream(&app, "/v1/events/stream?interactionsOnly=true").await;
    let mut all = open_stream(&app, "/v1/events/stream").await;
    assert!(
        !ends_within(&mut global, Duration::from_millis(200)).await,
        "a live stream stays open until shutdown"
    );
    shutdown.close();
    assert!(ends_within(&mut global, Duration::from_secs(5)).await);
    assert!(ends_within(&mut all, Duration::from_secs(5)).await);

    let (status, body) = get(&app, "/v1/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], json!("unavailable"));
    // A stream opened while shutting down ends at once.
    let mut late = open_stream(&app, "/v1/events/stream").await;
    assert!(ends_within(&mut late, Duration::from_secs(2)).await);
}
