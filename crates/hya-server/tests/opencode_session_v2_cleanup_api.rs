#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef, SessionId};
use hya_provider::ProviderRouter;
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-session-v2-cleanup-api";

async fn state() -> AppState {
    let router = Arc::new(ProviderRouter::new());
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        tools,
        perm,
        EventBus::default(),
    ));
    AppState::new(
        engine,
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

async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
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

#[tokio::test]
async fn opencode_v2_empty_cleanup_deletes_empty_session_after_hidden_list_fallback() {
    let state = state().await;
    let app = router(state.clone());
    let (status, created) = post_json(
        app.clone(),
        "/api/session",
        json!({"location": {"directory": WORKDIR}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");

    let (status, listed) = get_json(app.clone(), "/api/session?limit=10".to_string()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == session)
    );
    let (status, _) = get_json(app.clone(), format!("/api/session/{session}")).await;
    assert_eq!(status, StatusCode::OK);

    let session_id: SessionId = session.parse().unwrap();
    assert!(
        state
            .engine
            .cleanup_empty_unnamed_session(session_id)
            .await
            .unwrap()
    );

    let (status, _) = get_json(app, format!("/api/session/{session}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
