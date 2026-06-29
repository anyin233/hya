#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, MessageId, ModelRef, SessionId};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-session-v2-errors-api";

async fn state() -> AppState {
    let providers =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(Vec::new()))));
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

async fn post_json(app: axum::Router, uri: String, body: Option<Value>) -> (StatusCode, Value) {
    let mut request = Request::builder().method("POST").uri(uri);
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.oneshot(request.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn patch_json(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
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

async fn delete_json(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
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
async fn opencode_v2_session_missing_routes_return_typed_not_found_errors() {
    let app = router(state().await);
    let missing = SessionId::new().to_string();
    let expected = json!({
        "_tag": "SessionNotFoundError",
        "sessionID": missing,
        "message": format!("Session not found: {missing}"),
    });

    for uri in [
        format!("/api/session/{missing}"),
        format!("/api/session/{missing}/message"),
        format!("/api/session/{missing}/context"),
    ] {
        let (status, body) = get_json(app.clone(), uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, expected);
    }

    for uri in [
        format!("/api/session/{missing}/compact"),
        format!("/api/session/{missing}/wait"),
    ] {
        let (status, body) = post_json(app.clone(), uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, expected);
    }

    let (status, body) = patch_json(
        app.clone(),
        format!("/api/session/{missing}"),
        json!({"title": "never"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, expected);

    let (status, body) = delete_json(app.clone(), format!("/api/session/{missing}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, expected);

    let (status, body) = post_json(
        app.clone(),
        format!("/api/session/{missing}/prompt"),
        Some(json!({"prompt": {"text": "hello"}})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, expected);

    for (uri, body) in [
        (
            format!("/api/session/{missing}/init"),
            json!({
                "messageID": MessageId::new().to_string(),
                "providerID": "fake",
                "modelID": "fake",
            }),
        ),
        (
            format!("/api/session/{missing}/agent"),
            json!({"agent": "build"}),
        ),
        (
            format!("/api/session/{missing}/model"),
            json!({"model": {"providerID": "fake", "id": "fake"}}),
        ),
    ] {
        let (status, body) = post_json(app.clone(), uri, Some(body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, expected);
    }
}
