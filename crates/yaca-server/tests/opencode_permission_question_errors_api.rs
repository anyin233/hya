#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-permission-question-errors";

async fn state() -> AppState {
    let provider = Arc::new(FakeProvider::scripted(vec![]));
    let providers = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        providers,
        tools,
        permission,
        EventBus::default(),
    ));
    let agent = Arc::new(AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: WORKDIR.into(),
        reasoning: None,
    });
    engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: WORKDIR.to_string(),
        })
        .await
        .unwrap();
    AppState::new(engine, agent)
}

async fn request(app: axum::Router, method: &str, uri: String, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

#[tokio::test]
async fn opencode_v2_permission_and_question_missing_requests_return_typed_errors() {
    let app = router(state().await);
    let sessions = request(app.clone(), "GET", "/api/session".to_string(), Value::Null)
        .await
        .1;
    let session = sessions["data"][0]["id"].as_str().expect("session id");

    let (status, permission) = request(
        app.clone(),
        "POST",
        format!("/api/session/{session}/permission/per_missing/reply"),
        json!({"reply": "once"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(permission["_tag"], "PermissionNotFoundError");
    assert_eq!(permission["requestID"], "per_missing");
    assert_eq!(
        permission["message"],
        "Permission request not found: per_missing"
    );

    let (status, question) = request(
        app.clone(),
        "POST",
        format!("/api/session/{session}/question/que_missing/reply"),
        json!({"answers": []}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(question["_tag"], "QuestionNotFoundError");
    assert_eq!(question["requestID"], "que_missing");
    assert_eq!(
        question["message"],
        "Question request not found: que_missing"
    );

    let (status, question) = request(
        app,
        "POST",
        format!("/api/session/{session}/question/que_missing/reject"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(question["_tag"], "QuestionNotFoundError");
    assert_eq!(question["requestID"], "que_missing");
}
