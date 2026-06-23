#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, CompactionConfig, EventBus, ModelSummarizer, SessionEngine};
use yaca_proto::{AgentName, FinishReason, ModelRef, SessionId};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-session-v2-compact-api";

async fn state() -> AppState {
    let provider = FakeProvider::scripted(vec![
        FakeStep::Text("CONDENSED v2 summary".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let summarizer = Arc::new(ModelSummarizer::new(router.clone(), ModelRef::new("fake")));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default())
        .with_compaction(summarizer, CompactionConfig::default());
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn post_json(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    let resp = app
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
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn post_empty(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn get_json(app: axum::Router, uri: String) -> (StatusCode, Value) {
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
    let status = resp.status();
    (status, body_json(resp).await)
}

#[tokio::test]
async fn opencode_v2_session_compact_injects_system_summary_when_available() {
    let app = router(state().await);
    let requested = SessionId::new().to_string();
    let (status, _) = post_json(
        app.clone(),
        "/api/session".to_string(),
        json!({"id": requested, "location": {"directory": WORKDIR}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = post_json(
        app.clone(),
        format!("/api/session/{requested}/prompt"),
        json!({"prompt": {"text": "summarize this later"}, "resume": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, compact) =
        post_empty(app.clone(), format!("/api/session/{requested}/compact")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(compact, Value::Null);

    let (status, context) = get_json(app, format!("/api/session/{requested}/context")).await;
    assert_eq!(status, StatusCode::OK);
    let summary = context["data"]
        .as_array()
        .expect("context")
        .iter()
        .find(|message| message["type"] == "system")
        .expect("summary message");
    assert!(
        summary["text"]
            .as_str()
            .expect("summary text")
            .contains("CONDENSED v2 summary")
    );
}
