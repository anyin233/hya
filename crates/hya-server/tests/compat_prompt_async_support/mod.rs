#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::api::CreateSessionResponse;
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::AppState;
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

pub const WORKDIR: &str = "/tmp/hya-compat-prompt-async-api";

pub async fn state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::Text("Async title".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        vec![
            FakeStep::Text("async answer".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    state_with_router(ProviderRouter::new().with(Arc::new(provider)), "fake").await
}

pub async fn state_with_router(providers: ProviderRouter, model: &str) -> AppState {
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

pub async fn shell_state() -> AppState {
    std::fs::create_dir_all(WORKDIR).unwrap();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());
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

pub async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

pub async fn create_session(app: axum::Router) -> String {
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
    created.session.to_string()
}

pub async fn get_messages(app: axum::Router, session: &str) -> Value {
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

pub async fn post_prompt_async(app: axum::Router, session: &str, text: &str) -> StatusCode {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/session/{session}/prompt_async"))
            .header("content-type", "application/json")
            .body(Body::from(json!({"text": text}).to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

pub async fn wait_until_busy(app: axum::Router, session: &str) {
    for _ in 0..100 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/session/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        if body_json(resp).await[session]["type"] == "busy" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("session did not become busy");
}
