//! Request admission on the TCP listener (docs/protocol/README.md "Allowed
//! Host names"): only allowed Host names reach the routes (DNS rebinding),
//! a network request without a Host is refused, and the loopback-only
//! relay control fails closed when the client address is unknown.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, HostPolicy, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tower::ServiceExt as _;

async fn state(hosts: HostPolicy) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    )
    .with_allowed_hosts(hosts)
}

/// Serve the router on a loopback TCP listener, like `hya serve`.
async fn serve(hosts: HostPolicy) -> SocketAddr {
    let app = router(state(hosts).await);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    addr
}

/// Send `head` (a raw HTTP/1.x request head) and return `(status, body)`.
async fn raw(addr: SocketAddr, head: &str) -> (u16, Value) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

fn get(path: &str, host: Option<&str>, extra: &str) -> String {
    let host = host.map_or_else(String::new, |host| format!("Host: {host}\r\n"));
    format!("GET {path} HTTP/1.1\r\n{host}{extra}Connection: close\r\n\r\n")
}

#[tokio::test]
async fn loopback_host_names_reach_the_routes() {
    let addr = serve(HostPolicy::loopback()).await;
    for host in [
        format!("127.0.0.1:{}", addr.port()),
        format!("localhost:{}", addr.port()),
        "localhost".to_owned(),
        "[::1]:9".to_owned(),
    ] {
        let (status, body) = raw(addr, &get("/v1/health", Some(&host), "")).await;
        assert_eq!(status, 200, "{host}: {body}");
    }
}

#[tokio::test]
async fn a_rebound_host_name_cannot_read_the_relay_link_or_anything_else() {
    let addr = serve(HostPolicy::loopback()).await;
    for path in ["/v1/relay/link", "/v1/health", "/v1/sessions", "/nope"] {
        let (status, body) = raw(
            addr,
            &get(path, Some(&format!("evil.example:{}", addr.port())), ""),
        )
        .await;
        assert_eq!(status, 403, "{path}: {body}");
        assert_eq!(body["error"]["code"], "permission_denied", "{path}");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains("\"evil.example:"), "{message}");
        assert!(message.contains("--allow-host"), "{message}");
    }
}

#[tokio::test]
async fn a_cors_preflight_with_a_foreign_host_is_refused_before_cors_answers() {
    let addr = serve(HostPolicy::loopback()).await;
    let head = "OPTIONS /v1/relay/link HTTP/1.1\r\nHost: evil.example\r\nOrigin: http://evil.example\r\nAccess-Control-Request-Method: GET\r\nConnection: close\r\n\r\n";
    let (status, body) = raw(addr, head).await;
    assert_eq!(status, 403, "{body}");
}

#[tokio::test]
async fn a_request_without_a_host_is_refused_on_the_listener() {
    let addr = serve(HostPolicy::loopback()).await;
    let (status, body) = raw(addr, "GET /v1/health HTTP/1.0\r\n\r\n").await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["code"], "permission_denied");
}

#[tokio::test]
async fn allowed_hosts_extend_the_loopback_names() {
    let addr = serve(HostPolicy::with_hosts(["hya.example.lan"]).unwrap()).await;
    let (status, body) = raw(addr, &get("/v1/health", Some("HYA.example.lan:8080"), "")).await;
    assert_eq!(status, 200, "{body}");
    let (status, _) = raw(addr, &get("/v1/health", Some("other.example.lan"), "")).await;
    assert_eq!(status, 403);
    let (status, body) = raw(addr, &get("/v1/health", Some("other.example.lan"), "")).await;
    assert_eq!(status, 403);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("hya.example.lan"),
        "{body}"
    );
}

#[tokio::test]
async fn relay_control_without_a_known_client_address_fails_closed() {
    // An in-process request (no `ConnectInfo`) names no peer: refused.
    let app = router(state(HostPolicy::loopback()).await);
    let response = app
        .oneshot(
            axum::http::Request::get("/v1/relay/link")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    // Health is not loopback-only and passes in process.
    let app = router(state(HostPolicy::loopback()).await);
    let response = app
        .oneshot(
            axum::http::Request::get("/v1/health")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}
