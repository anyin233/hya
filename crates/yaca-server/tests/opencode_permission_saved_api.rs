#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{Action, PermissionPlane, PermissionRules, Resource, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-permission-saved-api";

async fn state() -> (
    axum::Router,
    PermissionPlane,
    Arc<SessionEngine>,
    Arc<AgentSpec>,
) {
    let (permission, permission_rx) = PermissionPlane::new(PermissionRules::default());
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        providers,
        tools,
        permission.clone(),
        EventBus::default(),
    ));
    let agent = Arc::new(AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: WORKDIR.into(),
        reasoning: None,
    });
    let app = router(
        AppState::new(engine.clone(), agent.clone()).with_permission_requests(permission_rx),
    );
    (app, permission, engine, agent)
}

async fn create_session(engine: &SessionEngine, agent: &AgentSpec) -> String {
    engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: WORKDIR.to_string(),
        })
        .await
        .unwrap()
        .to_string()
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn request(
    app: axum::Router,
    method: &str,
    uri: String,
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
    (status, body_json(resp).await)
}

async fn wait_for_permission(app: axum::Router, session: &str) -> String {
    for _ in 0..100 {
        let (status, body) = request(
            app.clone(),
            "GET",
            format!("/api/session/{session}/permission"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if let Some(id) = body["data"][0]["id"].as_str() {
            return id.to_string();
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("permission request did not appear");
}

#[tokio::test]
async fn opencode_permission_saved_lists_and_removes_always_reply() {
    let (app, permission, engine, agent) = state().await;
    let session = create_session(&engine, &agent).await;
    let task_session = session.parse().unwrap();
    let task = tokio::spawn(async move {
        permission
            .for_session(task_session)
            .assert(Action::Bash, Resource::Command("pwd".to_string()))
            .await
    });

    let request_id = wait_for_permission(app.clone(), &session).await;
    let (status, _) = request(
        app.clone(),
        "POST",
        format!("/api/session/{session}/permission/{request_id}/reply"),
        Some(json!({"reply": "always"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    task.await.unwrap().unwrap();

    let (status, saved) = request(
        app.clone(),
        "GET",
        "/api/permission/saved".to_string(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(saved["data"][0]["id"].as_str().unwrap().starts_with("psv_"));
    assert_eq!(saved["data"][0]["projectID"], "global");
    assert_eq!(saved["data"][0]["action"], "bash");
    assert_eq!(saved["data"][0]["resource"], "*");

    let (status, filtered) = request(
        app.clone(),
        "GET",
        "/api/permission/saved?projectID=other".to_string(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(filtered["data"], json!([]));

    let id = saved["data"][0]["id"].as_str().unwrap();
    let (status, _) = request(
        app.clone(),
        "DELETE",
        format!("/api/permission/saved/{id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, saved) = request(app, "GET", "/api/permission/saved".to_string(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["data"], json!([]));
}
