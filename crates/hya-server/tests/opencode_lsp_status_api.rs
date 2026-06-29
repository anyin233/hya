#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{
    LspError, LspPlane, LspProvider, LspRequest, PermissionPlane, PermissionRules, ToolRegistry,
};
use serde_json::{Value, json};
use tower::ServiceExt;

struct ConnectedLsp;

#[async_trait]
impl LspProvider for ConnectedLsp {
    async fn has_clients(&self, _file: &Path) -> Result<bool, LspError> {
        Ok(true)
    }

    async fn execute(&self, _request: LspRequest) -> Result<Vec<Value>, LspError> {
        Ok(Vec::new())
    }
}

struct RecordingLsp {
    seen: Arc<Mutex<Vec<PathBuf>>>,
}

#[async_trait]
impl LspProvider for RecordingLsp {
    async fn has_clients(&self, _file: &Path) -> Result<bool, LspError> {
        Ok(false)
    }

    async fn execute(&self, _request: LspRequest) -> Result<Vec<Value>, LspError> {
        Ok(Vec::new())
    }

    async fn status(&self, workdir: &Path) -> Result<Vec<Value>, LspError> {
        self.seen.lock().unwrap().push(workdir.to_path_buf());
        Ok(Vec::new())
    }
}

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-opencode-lsp-status-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: PathBuf, lsp: LspPlane) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine =
        SessionEngine::new(store, providers, tools, perm, EventBus::default()).with_lsp(lsp);
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir,
            reasoning: None,
        }),
    )
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn opencode_lsp_status_reports_connected_provider() {
    let app = router(state(tempdir(), LspPlane::new(Arc::new(ConnectedLsp))).await);

    let (status, body) = get_json(app, "/lsp").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!([{
            "id": "lsp",
            "name": "lsp",
            "root": "",
            "status": "connected"
        }])
    );
}

#[tokio::test]
async fn opencode_lsp_status_uses_workspace_routed_directory() {
    let default = tempdir();
    let scoped = tempdir().join("scoped dir");
    std::fs::create_dir_all(&scoped).unwrap();
    let scoped = std::fs::canonicalize(scoped).unwrap();
    let encoded_scoped = scoped.to_string_lossy().replace(' ', "%20");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = router(
        state(
            default,
            LspPlane::new(Arc::new(RecordingLsp { seen: seen.clone() })),
        )
        .await,
    );

    let (status, _body) = get_json(app, &format!("/lsp?directory={encoded_scoped}")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen.lock().unwrap().as_slice(), &[scoped]);
}
