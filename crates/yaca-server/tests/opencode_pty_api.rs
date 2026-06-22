#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-pty-api";

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

async fn request(
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

#[tokio::test]
async fn opencode_v2_pty_routes_report_shells_and_empty_sessions() {
    let app = router(state().await);

    let (status, shells) = request(app.clone(), "GET", "/api/pty/shells", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        shells.as_array().expect("shells").iter().any(|shell| {
            shell["acceptable"] == true
                && shell["path"]
                    .as_str()
                    .is_some_and(|path| path.starts_with('/'))
                && shell["name"].as_str().is_some_and(|name| !name.is_empty())
        }),
        "at least one executable shell should be discoverable"
    );

    let (status, sessions) = request(app.clone(), "GET", "/api/pty", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sessions, json!([]));

    let (status, _) = request(app.clone(), "GET", "/api/pty/pty_missing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = request(app, "POST", "/api/pty", Some(json!({}))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}
