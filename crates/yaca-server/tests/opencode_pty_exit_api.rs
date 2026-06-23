#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

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

const WORKDIR: &str = "/tmp/yaca-opencode-pty-exit-api";

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

async fn wait_for_exit(app: axum::Router, id: &str) -> Value {
    for _ in 0..50 {
        let (status, found) = request(app.clone(), "GET", &format!("/api/pty/{id}"), None).await;
        assert_eq!(status, StatusCode::OK);
        if found["data"]["status"] == "exited" {
            return found["data"].clone();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("pty did not exit");
}

#[tokio::test]
async fn opencode_pty_legacy_hides_exited_sessions() {
    let app = router(state().await);
    let (status, created) = request(
        app.clone(),
        "POST",
        "/pty",
        Some(json!({
            "command": "/bin/sh",
            "args": ["-c", "exit 7"],
            "title": "short lived"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = created["id"].as_str().unwrap();
    let exited = wait_for_exit(app.clone(), id).await;
    assert_eq!(exited["status"], "exited");
    assert_eq!(exited["exitCode"], 7);

    let (status, legacy_get) = request(app.clone(), "GET", &format!("/pty/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(legacy_get["_tag"], "PtyNotFoundError");

    let (status, legacy_list) = request(app.clone(), "GET", "/pty", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(legacy_list, json!([]));

    let (status, api_list) = request(app.clone(), "GET", "/api/pty", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(api_list["data"][0]["id"], id);
    assert_eq!(api_list["data"][0]["status"], "exited");

    let (status, _) = request(app.clone(), "DELETE", &format!("/pty/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = request(app, "DELETE", &format!("/api/pty/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
