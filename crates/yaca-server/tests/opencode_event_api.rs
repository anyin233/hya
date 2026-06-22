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
    assert!(event.get("location").is_none());
    let session = event["properties"]["sessionID"].as_str().unwrap();
    assert_eq!(event["properties"]["info"]["id"], session);
    assert_eq!(event["properties"]["info"]["directory"], directory);
}

#[tokio::test]
async fn opencode_v2_event_route_streams_session_updated_properties() {
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

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);

    let created_event = read_sse_json(&mut stream).await;
    assert_eq!(created_event["type"], "session.created");
    let session = created_event["properties"]["sessionID"]
        .as_str()
        .unwrap()
        .to_string();

    let updated = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/session/{session}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"title": "Renamed"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);

    let updated_event = read_sse_json(&mut stream).await;
    assert_eq!(updated_event["type"], "session.updated");
    assert!(updated_event.get("location").is_none());
    assert_eq!(updated_event["properties"]["sessionID"], session);
    assert_eq!(updated_event["properties"]["info"]["id"], session);
    assert_eq!(updated_event["properties"]["info"]["title"], "Renamed");
}

#[tokio::test]
async fn opencode_v2_event_route_streams_message_updated_properties() {
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
    assert_eq!(read_sse_json(&mut stream).await["type"], "server.connected");

    let created = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created_event = read_sse_json(&mut stream).await;
    let session = created_event["properties"]["sessionID"].as_str().unwrap();

    let command = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/sessions/{session}/command"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "command": "init",
                        "arguments": "audit",
                        "text": "/init audit"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(command.status(), StatusCode::OK);

    let started = read_sse_json(&mut stream).await;
    assert_eq!(started["type"], "message.updated");
    assert_eq!(started["properties"]["sessionID"], session);
    let message = started["properties"]["info"]["id"].as_str().unwrap();
    assert!(!message.is_empty());
    assert_eq!(started["properties"]["info"]["role"], "user");
    assert!(started["properties"]["info"].get("finish").is_none());

    let assistant_finished = read_next_message(&mut stream, "assistant", Some("stop")).await;
    assert_eq!(assistant_finished["properties"]["sessionID"], session);
    assert_eq!(
        assistant_finished["properties"]["info"]["role"],
        "assistant"
    );
    assert_eq!(assistant_finished["properties"]["info"]["finish"], "stop");
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
    assert_eq!(event["properties"], json!({}));
}

async fn read_next_message(
    stream: &mut axum::body::BodyDataStream,
    role: &str,
    finish: Option<&str>,
) -> serde_json::Value {
    for _ in 0..24 {
        let event = read_sse_json(stream).await;
        let info = &event["properties"]["info"];
        let finish_matches = match finish {
            Some(finish) => info["finish"] == finish,
            None => info.get("finish").is_none(),
        };
        if event["type"] == "message.updated" && info["role"] == role && finish_matches {
            return event;
        }
    }
    panic!("message.updated role {role} not found");
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
