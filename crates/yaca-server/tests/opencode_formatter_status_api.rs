#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, FormatterStatus, router};
use yaca_store::SessionStore;
use yaca_tool::{
    FormatterError, FormatterPlane, FormatterProvider, PermissionPlane, PermissionRules,
    ToolRegistry,
};

async fn state(status: Vec<FormatterStatus>) -> AppState {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake-model"),
            system_prompt: "system prompt".to_string(),
            workdir: PathBuf::from("."),
            reasoning: None,
        }),
    )
    .with_formatter_status(status)
}

struct StaticFormatterProvider;

#[async_trait]
impl FormatterProvider for StaticFormatterProvider {
    async fn status(
        &self,
        _workdir: &std::path::Path,
    ) -> Result<Vec<FormatterStatus>, FormatterError> {
        Ok(vec![FormatterStatus::new(
            "prettier",
            vec![".ts".to_string()],
            true,
        )])
    }
}

async fn provider_state() -> AppState {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default())
        .with_formatter(FormatterPlane::new(Arc::new(StaticFormatterProvider)));
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake-model"),
            system_prompt: "system prompt".to_string(),
            workdir: PathBuf::from("."),
            reasoning: None,
        }),
    )
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn formatter_status_defaults_to_empty() {
    let app = router(state(Vec::new()).await);

    let (status, body) = get_json(app, "/formatter").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!([]));
}

#[tokio::test]
async fn formatter_status_returns_configured_entries() {
    let app = router(
        state(vec![FormatterStatus::new(
            "gofmt",
            vec![".go".to_string()],
            false,
        )])
        .await,
    );

    let (status, body) = get_json(app, "/formatter").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        serde_json::json!([
            {
                "name": "gofmt",
                "extensions": [".go"],
                "enabled": false
            }
        ])
    );
}

#[tokio::test]
async fn formatter_status_returns_provider_entries() {
    let app = router(provider_state().await);

    let (status, body) = get_json(app, "/formatter").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        serde_json::json!([
            {
                "name": "prettier",
                "extensions": [".ts"],
                "enabled": true
            }
        ])
    );
}
