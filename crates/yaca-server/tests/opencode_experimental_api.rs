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

const WORKDIR: &str = "/tmp/yaca-opencode-experimental-api";

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
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

async fn request_json(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = request(app, method, uri, body).await;
    let status = response.status();
    (status, body_json(response).await)
}

#[tokio::test]
async fn opencode_experimental_metadata_routes_return_safe_defaults() {
    let app = router(state().await);

    let (status, capabilities) =
        request_json(app.clone(), "GET", "/experimental/capabilities", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(capabilities["backgroundSubagents"], false);

    let (status, console) = request_json(app.clone(), "GET", "/experimental/console", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(console["consoleManagedProviders"], json!([]));
    assert_eq!(console["switchableOrgCount"], 0);

    let (status, orgs) = request_json(app.clone(), "GET", "/experimental/console/orgs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(orgs["orgs"], json!([]));

    let (status, _body) = request_json(
        app.clone(),
        "POST",
        "/experimental/console/switch",
        Some(json!({"accountID": "acct", "orgID": "org"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, resources) = request_json(app, "GET", "/experimental/resource", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resources, json!({}));
}

#[tokio::test]
async fn opencode_experimental_workspace_tool_session_and_sync_routes_return_safe_defaults() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    for uri in [
        "/experimental/workspace/adapter",
        "/experimental/workspace",
        "/experimental/workspace/status",
        "/experimental/tool?provider=opencode&model=test",
        "/experimental/tool/ids",
        "/experimental/worktree",
    ] {
        let (status, body) = request_json(app.clone(), "GET", uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_array().is_some());
    }

    let (status, sessions) = request_json(
        app.clone(),
        "GET",
        "/experimental/session?roots=false&archived=false",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sessions[0]["id"],
        format!("ses_{}", session.replace('-', ""))
    );

    let (status, backgrounded) = request_json(
        app.clone(),
        "POST",
        &format!("/experimental/session/{session}/background"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(backgrounded, json!(false));

    let (status, _body) = request_json(
        app.clone(),
        "POST",
        "/experimental/workspace",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = request_json(
        app.clone(),
        "DELETE",
        "/experimental/workspace/wrk_missing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!(true));

    let sync_list = request(
        app.clone(),
        "POST",
        "/experimental/workspace/sync-list",
        None,
    )
    .await;
    assert_eq!(sync_list.status(), StatusCode::NO_CONTENT);

    let (status, _body) = request_json(
        app.clone(),
        "POST",
        "/experimental/workspace/warp",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, history) =
        request_json(app.clone(), "POST", "/sync/history", Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history, json!([]));

    let (status, _body) = request_json(
        app.clone(),
        "POST",
        "/sync/replay",
        Some(json!({"directory": WORKDIR, "events": []})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = request_json(app.clone(), "POST", "/sync/start", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!(true));

    let (status, missing) = request_json(
        app.clone(),
        "POST",
        "/experimental/control-plane/move-session",
        Some(json!({
            "sessionID": "ses_00000000000000000000000000000000",
            "destination": { "directory": "/tmp/yaca-moved" },
            "moveChanges": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(missing["name"], "MoveSessionError");
    assert!(
        missing["data"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("Session not found"))
    );

    let moved = request(
        app,
        "POST",
        "/experimental/control-plane/move-session",
        Some(json!({
            "sessionID": format!("ses_{}", session.replace('-', "")),
            "destination": { "directory": "/tmp/yaca-moved" },
            "moveChanges": true
        })),
    )
    .await;
    assert_eq!(moved.status(), StatusCode::NO_CONTENT);
}
