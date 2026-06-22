#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::api::CreateSessionResponse;
use yaca_proto::{AgentName, FinishReason, ModelRef};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-prompt-async-api";

async fn state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("async answer".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    state_with_router(ProviderRouter::new().with(Arc::new(provider)), "fake").await
}

async fn state_with_router(providers: ProviderRouter, model: &str) -> AppState {
    let router = Arc::new(providers);
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new(model),
            system_prompt: "x".to_string(),
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_session(app: axum::Router) -> String {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"agent": "build", "model": "fake", "workdir": WORKDIR}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created: CreateSessionResponse = serde_json::from_value(body_json(resp).await).unwrap();
    format!("ses_{}", created.session.as_uuid().simple())
}

async fn get_messages(app: axum::Router, session: &str) -> Value {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/message"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await
}

#[tokio::test]
async fn opencode_prompt_async_returns_no_content_and_records_messages() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/session/{session}/prompt_async"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"text": "hello async"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let messages = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let body = get_messages(app.clone(), &session).await;
            if body[1]["parts"][0]["text"] == "async answer" {
                break body;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("async prompt completed");
    assert_eq!(messages[0]["parts"][0]["text"], "hello async");
    assert_eq!(messages[1]["parts"][0]["text"], "async answer");
}

#[tokio::test]
async fn opencode_prompt_async_publishes_session_error_event_on_background_failure() {
    let app = router(state_with_router(ProviderRouter::new(), "missing").await);
    let session = create_session(app.clone()).await;

    let event_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(event_resp.status(), StatusCode::OK);
    let mut stream = event_resp.into_body().into_data_stream();
    let connected = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("connected event")
        .expect("body chunk")
        .expect("valid chunk");
    assert!(
        String::from_utf8(connected.to_vec())
            .unwrap()
            .contains("server.connected")
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/session/{session}/prompt_async"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"text": "hello async"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let error_frame = tokio::time::timeout(Duration::from_secs(2), async {
        let mut combined = String::new();
        loop {
            let Some(chunk) = stream.next().await else {
                panic!("event stream ended before session.error");
            };
            let bytes = chunk.expect("body chunk");
            combined.push_str(std::str::from_utf8(&bytes).unwrap());
            if combined.contains("\"type\":\"session.error\"") {
                break combined;
            }
        }
    })
    .await
    .expect("session.error event");
    assert!(error_frame.contains(&format!("\"sessionID\":\"{session}\"")));
    assert!(error_frame.contains("\"name\":\"UnknownError\""));
    assert!(error_frame.contains("unknown provider for model: fake"));
}

#[tokio::test]
async fn opencode_prompt_async_publishes_session_status_events() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    let event_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(event_resp.status(), StatusCode::OK);
    let mut stream = event_resp.into_body().into_data_stream();
    let connected = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("connected event")
        .expect("body chunk")
        .expect("valid chunk");
    assert!(
        String::from_utf8(connected.to_vec())
            .unwrap()
            .contains("server.connected")
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/session/{session}/prompt_async"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"text": "hello async"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let frames = tokio::time::timeout(Duration::from_secs(2), async {
        let mut combined = String::new();
        loop {
            let Some(chunk) = stream.next().await else {
                panic!("event stream ended before status events");
            };
            let bytes = chunk.expect("body chunk");
            combined.push_str(std::str::from_utf8(&bytes).unwrap());
            let has_busy = combined.contains("\"type\":\"session.status\"")
                && combined.contains("\"status\":{\"type\":\"busy\"}");
            let has_idle = combined.contains("\"type\":\"session.status\"")
                && combined.contains("\"status\":{\"type\":\"idle\"}");
            if has_busy && has_idle {
                break combined;
            }
        }
    })
    .await
    .expect("session.status events");
    assert!(frames.contains(&format!("\"sessionID\":\"{session}\"")));
}
