#![allow(clippy::unwrap_used)]

use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

fn workdir() -> &'static str {
    static WORKDIR: OnceLock<String> = OnceLock::new();
    WORKDIR.get_or_init(|| {
        let dir = std::env::temp_dir().join("hya-compat-sync-projection-api");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    })
}

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
            workdir: workdir().into(),
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
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(
                    body.map_or_else(String::new, |value| value.to_string()),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, body)
}

#[tokio::test]
async fn compat_sync_replay_projects_created_session() {
    let app = router(state().await);
    let session = "ses_00000000000000000000000000000000";
    let (status, body) = request_json(
        app.clone(),
        "POST",
        "/sync/replay",
        Some(json!({
            "directory": workdir(),
            "events": [{
                "id": "evt_00000000000000000000000000",
                "aggregateID": session,
                "seq": 0,
                "type": "session.created",
                "data": {
                    "sessionID": session,
                    "info": {
                        "id": session,
                        "slug": session,
                        "projectID": "local",
                        "directory": workdir(),
                        "title": "Remote session",
                        "agent": "build",
                        "model": {"providerID": "fake", "id": "fake"},
                        "version": "0.0.0",
                        "time": {"created": 1, "updated": 2}
                    }
                }
            }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sessionID"], session);

    let (status, session_body) =
        request_json(app, "GET", &format!("/session/{session}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session_body["id"], session);
    assert_eq!(session_body["title"], "Remote session");
    assert_eq!(session_body["directory"], workdir());
}
