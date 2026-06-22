#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

fn tempdir() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-location-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: impl Into<std::path::PathBuf>) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("openai/gpt-5"),
            system_prompt: "x".to_string(),
            workdir: workdir.into(),
            reasoning: None,
        }),
    )
}

async fn get_json(app: axum::Router, uri: String, headers: &[(&str, &str)]) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let resp = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

#[tokio::test]
async fn opencode_location_query_and_headers_override_default_workdir() {
    let workdir = tempdir();
    let scoped = workdir.join("scoped dir");
    std::fs::create_dir_all(&scoped).unwrap();
    let scoped = std::fs::canonicalize(scoped).unwrap();
    let scoped_text = scoped.to_string_lossy();
    let encoded_scoped = scoped_text.replace(' ', "%20");
    let app = router(state(workdir).await);

    let (status, location) = get_json(
        app.clone(),
        format!("/api/location?location%5Bdirectory%5D={encoded_scoped}"),
        &[("x-opencode-workspace", "wrk_query")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(location["directory"], scoped_text.as_ref());
    assert_eq!(location["workspaceID"], "wrk_query");
    assert_eq!(location["project"]["directory"], scoped_text.as_ref());

    let (status, agents) = get_json(
        app,
        "/api/agent".to_string(),
        &[
            ("x-opencode-directory", &encoded_scoped),
            ("x-opencode-workspace", "wrk_header"),
            (header::ACCEPT.as_str(), "application/json"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agents["location"]["directory"], scoped_text.as_ref());
    assert_eq!(agents["location"]["workspaceID"], "wrk_header");
    assert_eq!(agents["data"][0]["id"], "build");
}
