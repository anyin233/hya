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
use hya_server::{AppState, ShutdownReason, router};
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
    shutdown.close(ShutdownReason::Stop);
    assert!(ends_within(&mut global, Duration::from_secs(5)).await);
    assert!(ends_within(&mut all, Duration::from_secs(5)).await);

    let (status, body) = get(&app, "/v1/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], json!("unavailable"));
    // A stream opened while shutting down ends at once.
    let mut late = open_stream(&app, "/v1/events/stream").await;
    assert!(ends_within(&mut late, Duration::from_secs(2)).await);
}

/// Every `data:` frame of an SSE body, read until the body ends (at most `wait`).
async fn frames_to_end(stream: &mut axum::body::BodyDataStream, wait: Duration) -> Vec<Value> {
    let mut text = String::new();
    let ended = tokio::time::timeout(wait, async {
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            text.push_str(&String::from_utf8_lossy(&bytes));
        }
    })
    .await
    .is_ok();
    assert!(ended, "the stream did not end: {text}");
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str(data.trim()).ok())
        .collect()
}

async fn create_session(app: &axum::Router) -> String {
    let body = json!({"agent": "build", "model": "fake", "workdir": std::env::temp_dir().to_string_lossy()});
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    value["session"]["id"].as_str().unwrap().to_owned()
}

fn assert_stopping_last(frames: &[Value], reason: &str, stream: &str) {
    let last = frames
        .last()
        .unwrap_or_else(|| panic!("{stream}: no frame"));
    assert_eq!(
        last["event"]["serverStopping"],
        json!({"reason": reason}),
        "{stream}: {frames:#?}"
    );
    assert!(last["event"].get("seq").is_none(), "live-only: {last}");
    assert!(
        last["event"].get("session").is_none(),
        "process-wide: {last}"
    );
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["event"]["serverStopping"].is_object())
            .count(),
        1,
        "{stream}: exactly one serverStopping frame: {frames:#?}"
    );
}

/// The server says why it goes away: every SSE stream (global, interactions
/// only, session) gets one live `serverStopping {reason}` frame as its last
/// frame, then ends — for each reason, and for a stream opened while closing.
#[tokio::test]
async fn the_shutdown_reason_is_the_last_sse_frame() {
    for (reason, name) in [
        (ShutdownReason::Stop, "stop"),
        (ShutdownReason::Restart, "restart"),
        (ShutdownReason::Signal, "signal"),
    ] {
        let state = base_state().await;
        let shutdown = state.streams();
        let app = router(state);
        let session = create_session(&app).await;
        let mut global = open_stream(&app, "/v1/events/stream").await;
        let mut asks = open_stream(&app, "/v1/events/stream?interactionsOnly=true").await;
        let mut own = open_stream(&app, &format!("/v1/sessions/{session}/events/stream")).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        shutdown.close(reason);
        assert_eq!(shutdown.reason(), Some(reason));
        for (label, stream) in [
            ("global", &mut global),
            ("interactionsOnly", &mut asks),
            ("session", &mut own),
        ] {
            let frames = frames_to_end(stream, Duration::from_secs(5)).await;
            assert_stopping_last(&frames, name, label);
        }
        let mut late = open_stream(&app, "/v1/events/stream").await;
        let frames = frames_to_end(&mut late, Duration::from_secs(2)).await;
        assert_stopping_last(&frames, name, "late");
    }
}

/// The first reason wins: a second `close` (another signal while draining)
/// does not change what clients were told.
#[tokio::test]
async fn the_first_shutdown_reason_wins() {
    let shutdown = hya_server::StreamShutdown::default();
    assert_eq!(shutdown.reason(), None);
    shutdown.close(ShutdownReason::Restart);
    shutdown.close(ShutdownReason::Signal);
    assert_eq!(shutdown.reason(), Some(ShutdownReason::Restart));
    assert_eq!(ShutdownReason::Restart.as_str(), "restart");
}

/// gRPC parity: `StreamGlobalEvents` and `StreamSessionEvents` end with the
/// same `ServerStopping` payload.
#[tokio::test]
async fn the_shutdown_reason_reaches_grpc_subscribers_before_the_stream_ends() {
    use hya_api::v1 as pb;

    let state = base_state().await;
    let shutdown = state.streams();
    let app = router(state.clone());
    let session = create_session(&app).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = tonic::transport::Server::builder()
        .add_service(pb::events_server::EventsServer::new(
            hya_server::V1Grpc::new(state),
        ))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    let channel = tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = pb::events_client::EventsClient::new(channel);
    let global = client
        .stream_global_events(pb::StreamGlobalEventsRequest {
            interactions_only: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let own = client
        .stream_session_events(pb::StreamSessionEventsRequest {
            session: session.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    tokio::time::sleep(Duration::from_millis(100)).await;
    shutdown.close(ShutdownReason::Stop);
    for (label, mut stream) in [("global", global), ("session", own)] {
        let mut last = None;
        let ended = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(frame) = stream.next().await {
                let Ok(frame) = frame else { break };
                if let Some(pb::stream_frame::Frame::Event(event)) = frame.frame {
                    last = event.payload;
                }
            }
        })
        .await
        .is_ok();
        assert!(ended, "{label}: the gRPC stream must end");
        match last {
            Some(pb::stream_event::Payload::ServerStopping(stopping)) => {
                assert_eq!(stopping.reason, "stop", "{label}");
            }
            other => panic!("{label}: last payload {other:?}"),
        }
    }
}
