#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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

const WORKDIR: &str = "/tmp/hya-opencode-control-api";

async fn state() -> AppState {
    let provider = Arc::new(FakeProvider::scripted(vec![]));
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, permission, EventBus::default());
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
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn put_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    request_json(app, "PUT", uri, Some(body)).await
}

async fn delete_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    request_json(app, "DELETE", uri, None).await
}

async fn request_json(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn temp_config_home() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-opencode-control-auth-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn opencode_log_route_accepts_sdk_log_entries() {
    let app = router(state().await);

    for level in ["debug", "info", "warn", "error"] {
        let (status, logged) = post_json(
            app.clone(),
            "/log?directory=/tmp&workspace=default",
            json!({
                "service": "test-suite",
                "level": level,
                "message": format!("hello from {level}"),
                "extra": { "scope": "opencode-control" }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{level}");
        assert_eq!(logged, json!(true), "{level}");
    }
}

#[tokio::test]
async fn opencode_auth_routes_persist_and_remove_hya_tokens() {
    let config_home = temp_config_home();
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &config_home) };
    let token_path = config_home.join("hya/auth/openai.yaml");
    let app = router(state().await);

    let (status, saved) = put_json(
        app.clone(),
        "/auth/openai",
        json!({
            "type": "api",
            "key": "sk-test",
            "metadata": { "source": "opencode" }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved, json!(true));
    let token = std::fs::read_to_string(&token_path).unwrap();
    assert_eq!(token, "token: \"sk-test\"\n");

    let (status, removed) = delete_json(app, "/auth/openai").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(removed, json!(true));
    assert!(!token_path.exists());
}
