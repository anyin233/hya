#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    LspError, LspOperation, LspPlane, LspProvider, LspRequest, PermissionPlane, PermissionRules,
    ToolRegistry,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tower::ServiceExt;

#[derive(Clone)]
struct FakeLsp {
    result: Vec<Value>,
    requests: Arc<Mutex<Vec<LspRequest>>>,
}

#[async_trait]
impl LspProvider for FakeLsp {
    async fn has_clients(&self, _file: &Path) -> Result<bool, LspError> {
        Ok(true)
    }

    async fn execute(&self, request: LspRequest) -> Result<Vec<Value>, LspError> {
        self.requests.lock().await.push(request);
        Ok(self.result.clone())
    }
}

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-opencode-find-symbol-{nanos}-{}",
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
async fn opencode_find_symbol_returns_lsp_workspace_symbols() {
    let workdir = tempdir();
    let symbol = json!({
        "name": "main",
        "kind": 12,
        "location": {
            "uri": "file:///tmp/project/src/main.rs",
            "range": {
                "start": {"line": 0, "character": 3},
                "end": {"line": 0, "character": 7}
            }
        }
    });
    let requests = Arc::new(Mutex::new(Vec::new()));
    let lsp = LspPlane::new(Arc::new(FakeLsp {
        result: vec![symbol.clone()],
        requests: requests.clone(),
    }));

    let (status, body) = get_json(
        router(state(workdir.clone(), lsp).await),
        "/find/symbol?query=main",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([symbol]));
    let calls = requests.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].operation, LspOperation::WorkspaceSymbol);
    assert_eq!(calls[0].file, workdir);
    assert_eq!(calls[0].query.as_deref(), Some("main"));
}
