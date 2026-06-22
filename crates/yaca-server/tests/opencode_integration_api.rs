#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-integration-api";

async fn state() -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
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

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
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
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

async fn request_status(app: axum::Router, method: Method, uri: &str, body: Value) -> StatusCode {
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
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
}

async fn request_json(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
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
                .header(header::CONTENT_TYPE, "application/json")
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
async fn opencode_v2_reference_and_integration_routes_return_empty_discovery() {
    let app = router(state().await);

    let (status, references) = get_json(app.clone(), "/api/reference").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(references["location"]["directory"], WORKDIR);
    assert_eq!(references["data"], serde_json::json!([]));

    let (status, integrations) = get_json(app.clone(), "/api/integration").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(integrations["location"]["directory"], WORKDIR);
    assert_eq!(integrations["data"], serde_json::json!([]));

    let (status, integration) = get_json(app, "/api/integration/github").await;
    assert_eq!(status, StatusCode::OK);
    assert!(integration["data"].is_null());
}

#[tokio::test]
async fn opencode_v2_integration_mutation_routes_match_empty_backend() {
    let app = router(state().await);

    let (status, error) = request_json(
        app.clone(),
        Method::POST,
        "/api/integration/missing/connect/key",
        serde_json::json!({"key": "test"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["_tag"], "InvalidRequestError");
    assert_eq!(error["kind"], "integration_authorization");

    let (status, error) = request_json(
        app.clone(),
        Method::POST,
        "/api/integration/missing/connect/oauth",
        serde_json::json!({"methodID": "missing", "inputs": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["_tag"], "InvalidRequestError");
    assert_eq!(error["kind"], "integration_authorization");

    let status = request_status(
        app.clone(),
        Method::GET,
        "/api/integration/attempt/con_missing",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let (status, error) = request_json(
        app.clone(),
        Method::POST,
        "/api/integration/attempt/con_missing/complete",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["_tag"], "InvalidRequestError");
    assert_eq!(error["kind"], "integration_authorization");

    let status = request_status(
        app.clone(),
        Method::DELETE,
        "/api/integration/attempt/con_missing",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let status = request_status(
        app.clone(),
        Method::DELETE,
        "/api/credential/cred_missing",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let status = request_status(
        app,
        Method::PATCH,
        "/api/credential/cred_missing",
        serde_json::json!({"label": "Work"}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
