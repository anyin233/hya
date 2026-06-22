#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-mcp-dynamic-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn server_command() -> Vec<String> {
    vec![
        "python3".to_string(),
        "-c".to_string(),
        r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if req["method"] == "initialize":
        result = {"capabilities": {}}
    elif req["method"] == "tools/list":
        result = {"tools": [{"name": "ping", "description": "Ping", "inputSchema": {"type": "object"}}]}
    else:
        result = {"content": {"ok": True}, "isError": False}
    print(json.dumps({"jsonrpc":"2.0", "id": req["id"], "result": result}), flush=True)
"#
        .to_string(),
    ]
}

async fn state() -> AppState {
    let provider = Arc::new(FakeProvider::scripted(vec![]));
    let providers = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, permission, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: tempdir(),
            reasoning: None,
        }),
    )
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn request(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Value,
) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn get(app: axum::Router, uri: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn post_empty(app: axum::Router, uri: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn opencode_mcp_add_connects_enabled_local_server() {
    let app = router(state().await);

    let response = request(
        app.clone(),
        Method::POST,
        "/mcp",
        json!({
            "name": "dynamic",
            "config": {
                "type": "local",
                "command": server_command(),
                "timeout": 1000,
                "enabled": true
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["dynamic"],
        json!({"status": "connected"})
    );

    let response = get(app, "/mcp").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["dynamic"],
        json!({"status": "connected"})
    );
}

#[tokio::test]
async fn opencode_mcp_connect_and_disconnect_update_dynamic_status() {
    let app = router(state().await);

    let response = request(
        app.clone(),
        Method::POST,
        "/mcp",
        json!({
            "name": "dynamic",
            "config": {
                "type": "local",
                "command": server_command(),
                "timeout": 1000,
                "enabled": false
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["dynamic"],
        json!({"status": "disabled"})
    );

    let response = post_empty(app.clone(), "/mcp/dynamic/connect").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!(true));

    let response = get(app.clone(), "/mcp").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["dynamic"],
        json!({"status": "connected"})
    );

    let response = post_empty(app.clone(), "/mcp/dynamic/disconnect").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!(true));

    let response = get(app, "/mcp").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["dynamic"],
        json!({"status": "disabled"})
    );
}
