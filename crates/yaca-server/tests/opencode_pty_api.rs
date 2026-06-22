#![allow(clippy::unwrap_used, clippy::expect_used)]

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

const WORKDIR: &str = "/tmp/yaca-opencode-pty-api";

async fn state() -> AppState {
    let provider = Arc::new(FakeProvider::scripted(vec![]));
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, permission, EventBus::default());
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

async fn request(
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
    let resp = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn opencode_pty_routes_report_shells_and_manage_session_metadata() {
    let app = router(state().await);

    let (status, shells) = request(app.clone(), "GET", "/api/pty/shells", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        shells.as_array().expect("shells").iter().any(|shell| {
            shell["acceptable"] == true
                && shell["path"]
                    .as_str()
                    .is_some_and(|path| path.starts_with('/'))
                && shell["name"].as_str().is_some_and(|name| !name.is_empty())
        }),
        "at least one executable shell should be discoverable"
    );

    let (status, sessions) = request(app.clone(), "GET", "/pty", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sessions, json!([]));

    let (status, created) = request(
        app.clone(),
        "POST",
        "/pty",
        Some(json!({
            "command": "/bin/sh",
            "args": ["-c", "sleep 30"],
            "title": "HTTP API PTY"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["title"], "HTTP API PTY");
    assert_eq!(created["command"], "/bin/sh");
    assert_eq!(created["cwd"], WORKDIR);
    assert_eq!(created["status"], "running");
    let id = created["id"].as_str().expect("pty id");

    let (status, listed) = request(app.clone(), "GET", "/pty", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().expect("pty list").len(), 1);

    let (status, found) = request(app.clone(), "GET", &format!("/pty/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["id"], id);

    let (status, updated) = request(
        app.clone(),
        "PUT",
        &format!("/pty/{id}"),
        Some(json!({"title": "renamed", "size": {"rows": 24, "cols": 80}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["title"], "renamed");

    let (status, removed) = request(app.clone(), "DELETE", &format!("/pty/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(removed, json!(true));

    let (status, _) = request(app.clone(), "GET", &format!("/pty/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = request(app, "POST", "/pty", Some(json!({"command": 1}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn opencode_v2_pty_routes_wrap_location_and_manage_session_metadata() {
    let app = router(state().await);

    let (status, empty) = request(app.clone(), "GET", "/api/pty", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["data"], json!([]));
    assert_eq!(empty["location"]["directory"], WORKDIR);

    let (status, created) = request(
        app.clone(),
        "POST",
        "/api/pty",
        Some(json!({
            "command": "/bin/sh",
            "args": ["-c", "sleep 30"],
            "title": "HTTP API V2 PTY"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["data"]["title"], "HTTP API V2 PTY");
    let id = created["data"]["id"].as_str().expect("pty id");

    let (status, found) = request(app.clone(), "GET", &format!("/api/pty/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["data"]["id"], id);

    let (status, _) = request(app.clone(), "DELETE", &format!("/api/pty/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, missing) = request(app.clone(), "GET", &format!("/api/pty/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["_tag"], "PtyNotFoundError");

    let (status, _) = request(app, "POST", "/api/pty/pty_missing/connect-token", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
