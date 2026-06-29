#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::Value;
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-integration-api";

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
async fn opencode_v2_reference_route_lists_configured_local_references() {
    std::fs::create_dir_all(format!("{WORKDIR}/docs")).unwrap();
    let app = router(state().await);

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        serde_json::json!({
            "references": {
                "docs": {
                    "path": "docs",
                    "description": "Project docs",
                    "hidden": true
                },
                "bad/name": "./ignored",
                "effect": "Effect-TS/effect"
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, references) = get_json(app, "/api/reference").await;

    assert_eq!(status, StatusCode::OK);
    let data = references["data"].as_array().unwrap();
    assert_eq!(data.len(), 2);
    let docs = data.iter().find(|item| item["name"] == "docs").unwrap();
    assert_eq!(docs["name"], "docs");
    assert_eq!(docs["description"], "Project docs");
    assert_eq!(docs["hidden"], true);
    assert_eq!(docs["source"]["type"], "local");
    assert!(docs["path"].as_str().unwrap().ends_with("/docs"));
    let effect = data.iter().find(|item| item["name"] == "effect").unwrap();
    assert_eq!(effect["source"]["type"], "git");
    assert_eq!(effect["source"]["repository"], "Effect-TS/effect");
    assert!(
        effect["path"]
            .as_str()
            .unwrap()
            .ends_with("/Effect-TS/effect")
    );
    assert!(data.iter().all(|item| item["name"] != "bad/name"));
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
