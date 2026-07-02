#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef, SessionId};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-compat-permission-question-errors";

async fn state() -> AppState {
    state_with_session().await.0
}

async fn state_with_session() -> (AppState, SessionId) {
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
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: WORKDIR.to_string(),
        })
        .await
        .unwrap();
    (AppState::new(engine, agent), session)
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
async fn compat_v2_permission_and_question_missing_requests_return_typed_errors() {
    let (state, session) = state_with_session().await;
    let app = router(state);
    let session = session.to_string();

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

#[tokio::test]
async fn compat_v2_permission_and_question_missing_sessions_return_typed_errors() {
    let app = router(state().await);
    let missing = SessionId::new().to_string();

    for (method, uri, body) in [
        (
            "GET",
            format!("/api/session/{missing}/permission"),
            Value::Null,
        ),
        (
            "POST",
            format!("/api/session/{missing}/permission/per_missing/reply"),
            json!({"reply": "once"}),
        ),
        (
            "GET",
            format!("/api/session/{missing}/question"),
            Value::Null,
        ),
        (
            "POST",
            format!("/api/session/{missing}/question/que_missing/reply"),
            json!({"answers": []}),
        ),
        (
            "POST",
            format!("/api/session/{missing}/question/que_missing/reject"),
            Value::Null,
        ),
    ] {
        let (status, body) = request(app.clone(), method, uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["_tag"], "SessionNotFoundError");
        assert_eq!(body["sessionID"], missing);
        assert_eq!(body["message"], format!("Session not found: {missing}"));
    }
}
