#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

async fn state(workdir: impl Into<PathBuf>) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: workdir.into(),
            reasoning: None,
        }),
    )
}

fn tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-{label}-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_session(app: axum::Router, workdir: &str) -> String {
    let response = request(
        app,
        "POST",
        "/sessions",
        Some(json!({"agent": "build", "model": "fake", "workdir": workdir})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await["session"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn opencode_control_plane_move_session_updates_session_directory() {
    let source = std::fs::canonicalize(tempdir("move-session-source")).unwrap();
    let destination = std::fs::canonicalize(tempdir("move-session-destination")).unwrap();
    let source_text = source.to_string_lossy().into_owned();
    let destination_text = destination.to_string_lossy().into_owned();
    let app = router(state(source).await);
    let session = create_session(app.clone(), &source_text).await;

    let moved = request(
        app.clone(),
        "POST",
        "/experimental/control-plane/move-session",
        Some(json!({
            "sessionID": session,
            "destination": { "directory": destination_text },
            "moveChanges": false
        })),
    )
    .await;
    assert_eq!(moved.status(), StatusCode::NO_CONTENT);

    let response = request(app, "GET", &format!("/session/{session}"), None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["directory"], destination_text);
}
