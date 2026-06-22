#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_mcp::{McpManager, McpServerConfig};
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
        "yaca-server-mcp-test-{nanos}-{}",
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

async fn request(app: axum::Router, uri: &str) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn opencode_mcp_status_reports_configured_servers() {
    let mut configs = BTreeMap::new();
    configs.insert(
        "bad".to_string(),
        McpServerConfig {
            command: vec!["definitely-not-yaca-mcp".to_string()],
            ..McpServerConfig::default()
        },
    );
    configs.insert(
        "disabled".to_string(),
        McpServerConfig {
            enabled: Some(false),
            ..McpServerConfig::default()
        },
    );
    configs.insert(
        "good".to_string(),
        McpServerConfig {
            command: server_command(),
            timeout_ms: Some(1000),
            ..McpServerConfig::default()
        },
    );
    let mcp = McpManager::connect_all(configs).await;
    let app = router(state().await.with_mcp_manager(mcp));

    let response = request(app, "/mcp").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(body["good"], json!({"status": "connected"}));
    assert_eq!(body["disabled"], json!({"status": "disabled"}));
    assert_eq!(body["bad"]["status"], "failed");
    assert!(body["bad"]["error"].as_str().is_some_and(|s| !s.is_empty()));
}
