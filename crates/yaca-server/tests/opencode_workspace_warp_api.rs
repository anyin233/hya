#![allow(clippy::unwrap_used)]

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

const WORKDIR: &str = "/tmp/yaca-opencode-workspace-warp-api";

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

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_session(app: axum::Router) -> String {
    let response = request(
        app,
        "POST",
        "/sessions",
        Some(json!({"agent": "build", "model": "fake", "workdir": WORKDIR})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["session"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn opencode_workspace_warp_detaches_existing_session_to_local_project() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    let response = request(
        app,
        "POST",
        "/experimental/workspace/warp",
        Some(json!({
            "id": null,
            "sessionID": format!("ses_{}", session.replace('-', "")),
            "copyChanges": false
        })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn opencode_workspace_warp_missing_workspace_returns_not_found() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    let response = request(
        app,
        "POST",
        "/experimental/workspace/warp",
        Some(json!({
            "id": "wrk_missing",
            "sessionID": format!("ses_{}", session.replace('-', "")),
            "copyChanges": false
        })),
    )
    .await;
    let status = response.status();
    let body = body_json(response).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["name"], "NotFoundError");
    assert_eq!(body["data"]["message"], "Workspace not found: wrk_missing");
}
