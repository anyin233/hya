#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::api::CreateSessionResponse;
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-tui-api";

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
async fn opencode_tui_publish_queues_control_requests() {
    let app = router(state().await);
    let event = json!({
        "type": "tui.prompt.append",
        "properties": { "text": "draft prompt" }
    });

    let (status, published) =
        request(app.clone(), "POST", "/tui/publish", Some(event.clone())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(published, json!(true));

    let (status, next) = request(app.clone(), "GET", "/tui/control/next", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(next["path"], "/tui/publish");
    assert_eq!(next["body"], event);

    let (status, accepted) = request(
        app,
        "POST",
        "/tui/control/response",
        Some(json!({ "ok": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(accepted, json!(true));
}

#[tokio::test]
async fn opencode_tui_direct_routes_queue_control_requests() {
    let app = router(state().await);
    let cases = [
        (
            "/tui/append-prompt",
            Some(json!({ "text": "from IDE" })),
            json!({ "text": "from IDE" }),
        ),
        ("/tui/open-help", None, Value::Null),
        ("/tui/open-sessions", None, Value::Null),
        ("/tui/open-themes", None, Value::Null),
        ("/tui/open-models", None, Value::Null),
        ("/tui/submit-prompt", None, Value::Null),
        ("/tui/clear-prompt", None, Value::Null),
        (
            "/tui/execute-command",
            Some(json!({ "command": "session_new" })),
            json!({ "command": "session_new" }),
        ),
        (
            "/tui/show-toast",
            Some(json!({
                "title": "Done",
                "message": "Task completed",
                "variant": "success",
                "duration": 1500
            })),
            json!({
                "title": "Done",
                "message": "Task completed",
                "variant": "success",
                "duration": 1500
            }),
        ),
    ];

    for (path, payload, expected_body) in cases {
        let (status, accepted) = request(app.clone(), "POST", path, payload).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(accepted, json!(true), "{path}");

        let (status, next) = request(app.clone(), "GET", "/tui/control/next", None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(next["path"], path, "{path}");
        assert_eq!(next["body"], expected_body, "{path}");
    }
}

#[tokio::test]
async fn opencode_tui_select_session_validates_and_queues_existing_sessions() {
    let app = router(state().await);

    let (status, _) = request(
        app.clone(),
        "POST",
        "/tui/select-session",
        Some(json!({ "sessionID": "not-a-session-id" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let missing = yaca_proto::SessionId::new().to_string();
    let (status, _) = request(
        app.clone(),
        "POST",
        "/tui/select-session",
        Some(json!({ "sessionID": missing })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, created) = request(
        app.clone(),
        "POST",
        "/sessions",
        Some(json!({ "agent": "build", "model": "fake", "workdir": WORKDIR })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let created: CreateSessionResponse = serde_json::from_value(created).unwrap();
    let session_id = created.session.to_string();

    let payload = json!({ "sessionID": session_id });
    let (status, accepted) = request(
        app.clone(),
        "POST",
        "/tui/select-session",
        Some(payload.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(accepted, json!(true));

    let (status, next) = request(app.clone(), "GET", "/tui/control/next", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(next["path"], "/tui/select-session");
    assert_eq!(next["body"], payload);
}
