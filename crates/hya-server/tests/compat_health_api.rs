#![allow(clippy::unwrap_used)]

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
use serde_json::{Value, json};
use tower::ServiceExt;

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
            workdir: "/tmp/hya-compat-health-api".into(),
            reasoning: None,
        }),
    )
}

#[tokio::test]
async fn compat_v2_health_route_reports_ready() {
    assert_health("/api/health").await;
}

#[tokio::test]
async fn compat_global_health_route_reports_ready() {
    assert_health("/global/health").await;
}

#[tokio::test]
async fn compat_cors_preflight_mirrors_origin_and_request_headers() {
    let resp = router(state().await)
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/global/config")
                .header(header::ORIGIN, "http://localhost:3000")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(
                    header::ACCESS_CONTROL_REQUEST_HEADERS,
                    "content-type, x-opencode-directory",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!([StatusCode::OK, StatusCode::NO_CONTENT].contains(&resp.status()));
    assert_eq!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok()),
        Some("http://localhost:3000")
    );
    let vary = resp
        .headers()
        .get(header::VARY)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(vary.contains("origin"));
    assert!(vary.contains("access-control-request-headers"));
}

async fn assert_health(uri: &str) {
    let resp = router(state().await)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["healthy"], true);
}

async fn request_json(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let request_body = if let Some(body) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&body).unwrap())
    } else {
        Body::empty()
    };
    let resp = app
        .oneshot(builder.body(request_body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, body)
}

#[tokio::test]
async fn compat_global_config_routes_store_runtime_config() {
    let app = router(state().await);

    let (status, body) = request_json(app.clone(), Method::GET, "/global/config", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({}));

    let (status, body) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        Some(json!({"username": "httpapi-global"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], "httpapi-global");

    let (status, body) = request_json(app.clone(), Method::GET, "/global/config", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], "httpapi-global");

    let (status, _body) = request_json(
        app,
        Method::PATCH,
        "/global/config",
        Some(json!({"username": 7})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn compat_global_dispose_route_returns_true() {
    let app = router(state().await);

    let (status, body) = request_json(app, Method::POST, "/global/dispose", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!(true));
}

#[tokio::test]
async fn compat_global_upgrade_rejects_invalid_target() {
    let app = router(state().await);

    let (status, body) = request_json(
        app,
        Method::POST,
        "/global/upgrade",
        Some(json!({"target": 1})),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"success": false, "error": "Invalid request body"})
    );
}

#[tokio::test]
async fn compat_global_upgrade_reports_unknown_installation() {
    let app = router(state().await);

    let (status, body) = request_json(app, Method::POST, "/global/upgrade", None).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"success": false, "error": "Unknown installation method"})
    );
}
