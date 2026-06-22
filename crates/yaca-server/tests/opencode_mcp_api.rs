#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
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

async fn request(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    let request_body = if let Some(body) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        Body::from(serde_json::to_vec(&body).unwrap())
    } else {
        Body::empty()
    };
    app.oneshot(builder.body(request_body).unwrap())
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

    let response = request(app, Method::GET, "/mcp", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(body["good"], json!({"status": "connected"}));
    assert_eq!(body["disabled"], json!({"status": "disabled"}));
    assert_eq!(body["bad"]["status"], "failed");
    assert!(body["bad"]["error"].as_str().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn opencode_mcp_add_accepts_disabled_local_server() {
    let app = router(state().await);

    let response = request(
        app.clone(),
        Method::POST,
        "/mcp",
        Some(json!({
            "name": "httpapi-disabled",
            "config": {
                "type": "local",
                "command": ["bun", "--version"],
                "enabled": false
            }
        })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["httpapi-disabled"], json!({"status": "disabled"}));

    let response = request(app, Method::GET, "/mcp", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["httpapi-disabled"], json!({"status": "disabled"}));
}

#[tokio::test]
async fn opencode_mcp_add_rejects_invalid_config() {
    let app = router(state().await);

    let response = request(
        app,
        Method::POST,
        "/mcp",
        Some(json!({
            "name": "httpapi-invalid",
            "config": { "type": "invalid" }
        })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn opencode_mcp_connect_disconnect_acknowledge_known_server() {
    let app = router(state().await);
    let added = request(
        app.clone(),
        Method::POST,
        "/mcp",
        Some(json!({
            "name": "added",
            "config": {
                "type": "local",
                "command": ["echo", "added"],
                "enabled": false
            }
        })),
    )
    .await;
    assert_eq!(added.status(), StatusCode::OK);

    let connected = request(app.clone(), Method::POST, "/mcp/added/connect", None).await;
    assert_eq!(connected.status(), StatusCode::OK);
    assert_eq!(body_json(connected).await, json!(true));

    let disconnected = request(app, Method::POST, "/mcp/added/disconnect", None).await;
    assert_eq!(disconnected.status(), StatusCode::OK);
    assert_eq!(body_json(disconnected).await, json!(true));
}

#[tokio::test]
async fn opencode_mcp_auth_routes_return_deterministic_responses_for_known_non_oauth_server() {
    let app = router(state().await);
    let added = request(
        app.clone(),
        Method::POST,
        "/mcp",
        Some(json!({
            "name": "demo",
            "config": {
                "type": "local",
                "command": ["echo", "demo"],
                "enabled": false
            }
        })),
    )
    .await;
    assert_eq!(added.status(), StatusCode::OK);

    for path in ["/mcp/demo/auth", "/mcp/demo/auth/authenticate"] {
        let response = request(app.clone(), Method::POST, path, None).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await,
            json!({"error": "MCP server demo does not support OAuth"})
        );
    }

    let response = request(app, Method::DELETE, "/mcp/demo/auth", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"success": true}));
}

#[tokio::test]
async fn opencode_mcp_routes_return_typed_not_found_for_missing_server() {
    let app = router(state().await);

    for (method, path, body) in [
        (Method::POST, "/mcp/missing/auth", None),
        (Method::POST, "/mcp/missing/auth/authenticate", None),
        (
            Method::POST,
            "/mcp/missing/auth/callback",
            Some(json!({"code": "code"})),
        ),
        (Method::DELETE, "/mcp/missing/auth", None),
        (Method::POST, "/mcp/missing/connect", None),
        (Method::POST, "/mcp/missing/disconnect", None),
    ] {
        let response = request(app.clone(), method, path, body).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            body_json(response).await,
            json!({
                "_tag": "McpServerNotFoundError",
                "name": "missing",
                "message": "MCP server not found: missing"
            })
        );
    }
}
