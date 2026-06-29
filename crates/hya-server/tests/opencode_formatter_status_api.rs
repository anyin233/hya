#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, FormatterStatus, router};
use hya_store::SessionStore;
use hya_tool::{
    FormatterError, FormatterPlane, FormatterProvider, PermissionPlane, PermissionRules,
    ToolRegistry,
};
use serde_json::Value;
use tower::ServiceExt;

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-server-formatter-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

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

struct RecordingFormatterProvider {
    seen: Arc<Mutex<Vec<PathBuf>>>,
}

#[async_trait]
impl FormatterProvider for RecordingFormatterProvider {
    async fn status(
        &self,
        workdir: &std::path::Path,
    ) -> Result<Vec<FormatterStatus>, FormatterError> {
        self.seen.lock().unwrap().push(workdir.to_path_buf());
        Ok(Vec::new())
    }
}

async fn provider_state() -> AppState {
    provider_state_with(PathBuf::from("."), Arc::new(StaticFormatterProvider)).await
}

async fn provider_state_with(workdir: PathBuf, provider: Arc<dyn FormatterProvider>) -> AppState {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default())
        .with_formatter(FormatterPlane::new(provider));
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake-model"),
            system_prompt: "system prompt".to_string(),
            workdir,
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

#[tokio::test]
async fn formatter_status_uses_workspace_routed_directory() {
    let default = tempdir();
    let scoped = tempdir().join("scoped dir");
    std::fs::create_dir_all(&scoped).unwrap();
    let scoped = std::fs::canonicalize(scoped).unwrap();
    let encoded_scoped = scoped.to_string_lossy().replace(' ', "%20");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = router(
        provider_state_with(
            default,
            Arc::new(RecordingFormatterProvider { seen: seen.clone() }),
        )
        .await,
    );

    let (status, _body) = get_json(app, &format!("/formatter?directory={encoded_scoped}")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen.lock().unwrap().as_slice(), &[scoped]);
}
