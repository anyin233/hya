#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-pty-connect-api";

async fn state() -> AppState {
    let provider = Arc::new(FakeProvider::scripted(vec![]));
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, permission, EventBus::default());
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

async fn request(app: axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

#[tokio::test]
async fn opencode_pty_connect_streams_process_io_over_websocket() {
    let app = router(state().await);
    let (status, created) = request(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri("/pty")
            .header("content-type", "application/json")
            .body(Body::from(json!({"command": "/bin/cat"}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = created["id"].as_str().expect("pty id");

    let (status, token) = request(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(format!("/pty/{id}/connect-token"))
            .header("x-opencode-ticket", "1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ticket = token["ticket"].as_str().expect("ticket");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let (mut socket, response) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/pty/{id}/connect?ticket={ticket}"))
            .await
            .unwrap();
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    let meta = socket.next().await.unwrap().unwrap();
    assert!(matches!(meta, Message::Binary(frame) if frame.first() == Some(&0)));

    socket
        .send(Message::Text("hello from websocket\n".to_string()))
        .await
        .unwrap();
    let echoed = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(echoed, Message::Text(text) if text.contains("hello from websocket")));
    let _ = socket.close(None).await;
    server.abort();
}
