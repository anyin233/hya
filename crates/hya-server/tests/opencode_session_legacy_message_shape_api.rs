#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::api::CreateSessionResponse;
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-legacy-message-shape-api";

async fn state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("assistant answer".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
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

async fn create_session(app: axum::Router) -> String {
    let (status, created) = post_json(
        app,
        "/sessions".to_string(),
        json!({"agent": "build", "model": "fake", "workdir": WORKDIR}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let created: CreateSessionResponse = serde_json::from_value(created).unwrap();
    created.session.to_string()
}

#[tokio::test]
async fn legacy_message_info_matches_opencode_required_shape() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;
    let (status, _) = post_json(
        app.clone(),
        format!("/sessions/{session}/prompt"),
        json!({"text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = get_json(app, format!("/session/{session}/message")).await;
    assert_eq!(status, StatusCode::OK);
    let user = &body[0]["info"];
    let assistant = &body[1]["info"];

    assert_eq!(user["agent"], "build");
    assert_eq!(
        user["model"],
        json!({"providerID": "hya", "modelID": "fake"})
    );
    assert!(user["time"]["created"].as_u64().is_some());
    assert_eq!(assistant["parentID"], user["id"]);
    assert_eq!(assistant["modelID"], "fake");
    assert_eq!(assistant["providerID"], "hya");
    assert_eq!(assistant["mode"], "build");
    assert_eq!(assistant["agent"], "build");
    assert_eq!(assistant["path"], json!({"cwd": WORKDIR, "root": WORKDIR}));
    assert_eq!(assistant["cost"], 0);
    assert_eq!(
        assistant["tokens"],
        json!({"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}})
    );
    assert!(assistant["time"]["created"].as_u64().is_some());
    assert!(
        assistant["time"]["completed"].as_u64().unwrap()
            >= assistant["time"]["created"].as_u64().unwrap()
    );
}

#[tokio::test]
async fn legacy_session_post_creates_opencode_session_info() {
    let app = router(state().await);

    let (status, body) = post_json(
        app,
        "/session".to_string(),
        json!({
            "agent": "build",
            "model": {"providerID": "hya", "id": "fake"},
            "location": {"directory": WORKDIR}
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["agent"], "build");
    assert_eq!(body["model"]["id"], "fake");
    assert_eq!(body["directory"], WORKDIR);
    assert!(body["id"].as_str().is_some_and(|id| {
        let suffix = id.strip_prefix("hysec_");
        suffix.is_some_and(|suffix| {
            suffix.len() == 20 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
    }));
}

#[tokio::test]
async fn legacy_session_message_post_accepts_opencode_prompt_parts() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    let (status, body) = post_json(
        app.clone(),
        format!("/session/{session}/message"),
        json!({"parts": [{"type": "text", "text": "hello from parts"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["info"]["sessionID"], session);
    assert_eq!(body["parts"][0]["type"], "text");
    assert_eq!(body["parts"][0]["text"], "hello from parts");

    let (status, messages) = get_json(app, format!("/session/{session}/message")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(messages[0]["parts"][0]["text"], "hello from parts");
    assert_eq!(messages[1]["parts"][1]["text"], "assistant answer");
}
