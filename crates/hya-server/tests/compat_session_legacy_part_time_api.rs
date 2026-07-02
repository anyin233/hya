#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-compat-legacy-part-time-api";

async fn state() -> AppState {
    let providers = Arc::new(
        ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![
            FakeStep::Reasoning("thinking".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ]))),
    );
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, permission, EventBus::default());
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

async fn request_json(app: axum::Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn legacy_message_parts_include_stream_times() {
    let app = router(state().await);
    let created = request_json(
        app.clone(),
        "POST",
        "/sessions",
        Some(json!({"agent": "build", "model": "fake", "workdir": WORKDIR})),
    )
    .await;
    let session = created["session"].as_str().unwrap();

    request_json(
        app.clone(),
        "POST",
        &format!("/sessions/{session}/prompt"),
        Some(json!({"text": "hello"})),
    )
    .await;
    let messages = request_json(app, "GET", &format!("/session/{session}/message"), None).await;

    let text_time = &messages[0]["parts"][0]["time"];
    assert!(text_time["start"].as_u64().is_some());
    assert!(text_time["end"].as_u64() >= text_time["start"].as_u64());

    let reasoning = messages[1]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["type"] == "reasoning")
        .expect("reasoning part");
    assert!(reasoning["time"]["start"].as_u64().is_some());
    assert!(reasoning["time"]["end"].as_u64() >= reasoning["time"]["start"].as_u64());
}
