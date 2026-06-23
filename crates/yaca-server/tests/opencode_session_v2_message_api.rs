#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, FinishReason, ModelRef};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-session-v2-message-api";

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

#[tokio::test]
async fn opencode_v2_session_message_route_paginates_projected_messages() {
    let app = router(state().await);
    let (status, created) = post_json(
        app.clone(),
        "/api/session".to_string(),
        json!({"location": {"directory": WORKDIR}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");

    let (status, _) = post_json(
        app.clone(),
        format!("/sessions/{session}/prompt"),
        json!({"text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, default_page) =
        get_json(app.clone(), format!("/api/session/{session}/message")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(default_page["data"][0]["type"], "assistant");
    assert_eq!(
        default_page["data"][0]["content"][0]["text"],
        "assistant answer"
    );
    assert_eq!(default_page["data"][1]["type"], "user");
    assert!(default_page["cursor"]["previous"].as_str().is_some());
    assert!(default_page["cursor"]["next"].as_str().is_some());

    let (status, first) = get_json(
        app.clone(),
        format!("/api/session/{session}/message?limit=1&order=asc"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["data"][0]["type"], "user");

    let cursor = first["cursor"]["next"].as_str().expect("next cursor");
    let decoded: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(cursor).expect("cursor b64")).unwrap();
    assert_eq!(decoded["id"], first["data"][0]["id"]);
    assert_eq!(decoded["time"], first["data"][0]["time"]["created"]);

    let (status, second) = get_json(
        app.clone(),
        format!("/api/session/{session}/message?limit=1&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["data"][0]["type"], "assistant");

    let (status, body) = get_json(
        app.clone(),
        format!("/api/session/{session}/message?order=asc&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({
            "_tag": "InvalidCursorError",
            "message": "Cursor cannot be combined with order",
        })
    );

    let (status, body) = get_json(
        app,
        format!("/api/session/{session}/message?cursor=invalid"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({
            "_tag": "InvalidCursorError",
            "message": "Invalid cursor",
        })
    );
}

#[tokio::test]
async fn opencode_v2_session_message_limit_zero_returns_all_messages() {
    let app = router(state().await);
    let (status, created) = post_json(
        app.clone(),
        "/api/session".to_string(),
        json!({"location": {"directory": WORKDIR}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");

    let (status, _) = post_json(
        app.clone(),
        format!("/sessions/{session}/prompt"),
        json!({"text": "hello"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, page) = get_json(
        app.clone(),
        format!("/api/session/{session}/message?limit=0"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["data"].as_array().expect("messages").len(), 2);

    let (status, page) = get_json(app, format!("/api/session/{session}/message?limit=201")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["data"].as_array().expect("messages").len(), 2);
}
