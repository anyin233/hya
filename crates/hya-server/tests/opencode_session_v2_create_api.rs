#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-session-v2-create-api";

async fn state() -> AppState {
    let router =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn post_session(app: axum::Router, body: Body) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

#[tokio::test]
async fn opencode_v2_session_create_accepts_empty_body() {
    let app = router(state().await);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["data"]["agent"], "build");
    assert_eq!(body["data"]["model"]["providerID"], "hya");
    assert_eq!(body["data"]["model"]["id"], "fake");
    assert_eq!(body["data"]["directory"], WORKDIR);
}

#[tokio::test]
async fn opencode_v2_session_create_treats_whitespace_body_as_empty() {
    let app = router(state().await);
    let (status, body) = post_session(app, Body::from(" \n\t ")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["agent"], "build");
    assert_eq!(body["data"]["model"]["providerID"], "hya");
    assert_eq!(body["data"]["model"]["id"], "fake");
    assert_eq!(body["data"]["directory"], WORKDIR);
}

#[tokio::test]
async fn opencode_v2_session_create_accepts_parent_id() {
    let app = router(state().await);
    let (status, parent) = post_session(app.clone(), Body::empty()).await;
    assert_eq!(status, StatusCode::OK);

    let parent_id = parent["data"]["id"].as_str().unwrap();
    let (status, child) =
        post_session(app, Body::from(json!({"parentID": parent_id}).to_string())).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(child["data"]["parentID"], parent_id);
}
