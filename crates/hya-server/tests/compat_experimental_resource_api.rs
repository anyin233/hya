#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_mcp::{McpManager, McpServerConfig};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-compat-experimental-resource-api";

fn resource_server_command() -> Vec<String> {
    vec![
        "python3".to_string(),
        "-c".to_string(),
        r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    if req["method"] == "initialize":
        result = {"capabilities": {"resources": {}}}
    elif req["method"] == "tools/list":
        result = {"tools": []}
    elif req["method"] == "resources/list":
        result = {"resources": [{"name": "Project Notes", "uri": "file:///notes.md", "description": "Notes", "mimeType": "text/markdown"}]}
    else:
        result = {"content": {"ok": True}, "isError": False}
    print(json.dumps({"jsonrpc":"2.0", "id": req["id"], "result": result}), flush=True)
"#
        .to_string(),
    ]
}

async fn state() -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, permission, EventBus::default());
    let mut configs = BTreeMap::new();
    configs.insert(
        "docs/server".to_string(),
        McpServerConfig {
            command: resource_server_command(),
            timeout_ms: Some(1000),
            ..McpServerConfig::default()
        },
    );
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
    .with_mcp_manager(McpManager::connect_all(configs).await)
}

async fn request_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn compat_experimental_resource_lists_connected_mcp_resources() {
    let app = router(state().await);

    let (status, resources) = request_json(app, "/experimental/resource").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        resources["docs_server:Project_Notes"],
        json!({
            "name": "Project Notes",
            "uri": "file:///notes.md",
            "description": "Notes",
            "mimeType": "text/markdown",
            "client": "docs/server"
        })
    );
}
