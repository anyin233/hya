#![allow(clippy::unwrap_used)]

use std::sync::Arc;

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

const WORKDIR: &str = "/tmp/yaca-opencode-experimental-session-filter-api";

async fn state() -> AppState {
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
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn request_json(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn create_session(app: axum::Router) -> String {
    let (status, body) = request_json(
        app,
        "POST",
        "/sessions",
        Some(json!({"agent": "build", "model": "fake", "workdir": WORKDIR})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["session"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn experimental_session_list_filters_archived_sessions_by_default() {
    let app = router(state().await);
    let active = create_session(app.clone()).await;
    let archived = create_session(app.clone()).await;
    let active_id = opencode_id(&active);
    let archived_id = opencode_id(&archived);

    let (status, _) = request_json(
        app.clone(),
        "PATCH",
        &format!("/session/{archived}"),
        Some(json!({"time": {"archived": 42}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for uri in [
        "/experimental/session",
        "/experimental/session?archived=false",
    ] {
        let (status, sessions) = request_json(app.clone(), "GET", uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(session_ids(&sessions).contains(&active_id));
        assert!(!session_ids(&sessions).contains(&archived_id));
    }

    let (status, archived_sessions) =
        request_json(app, "GET", "/experimental/session?archived=true", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(session_ids(&archived_sessions).contains(&archived_id));
}

fn opencode_id(id: &str) -> String {
    format!("ses_{}", id.replace('-', ""))
}

fn session_ids(sessions: &Value) -> Vec<String> {
    sessions
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|session| session["id"].as_str().map(ToString::to_string))
        .collect()
}
