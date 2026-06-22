#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
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
        "yaca-server-catalog-location-test-{nanos}-{}",
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
async fn opencode_catalog_routes_honor_location_query_and_headers() {
    let workdir = tempdir();
    let scoped = workdir.join("provider scoped");
    std::fs::create_dir_all(&scoped).unwrap();
    let scoped = std::fs::canonicalize(scoped).unwrap();
    let scoped_text = scoped.to_string_lossy();
    let encoded_scoped = scoped_text.replace(' ', "%20");
    let app = router(state(workdir).await);

    let (status, providers) = get_json(
        app.clone(),
        format!("/api/provider?location%5Bdirectory%5D={encoded_scoped}&location%5Bworkspace%5D=wrk_catalog"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(providers["location"]["directory"], scoped_text.as_ref());
    assert_eq!(providers["location"]["workspaceID"], "wrk_catalog");
    assert_eq!(providers["data"][0]["id"], "openai");

    let (status, models) = get_json(
        app,
        "/api/model".to_string(),
        &[
            ("x-opencode-directory", &encoded_scoped),
            ("x-opencode-workspace", "wrk_model"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models["location"]["directory"], scoped_text.as_ref());
    assert_eq!(models["location"]["workspaceID"], "wrk_model");
    assert_eq!(models["data"][0]["id"], "gpt-5");
}
