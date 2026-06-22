#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::api::{CreateSessionResponse, PromptResponse};
use yaca_proto::{AgentName, FinishReason, ModelRef, SessionId};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-session-revert-api";

async fn state() -> AppState {
    std::fs::create_dir_all(WORKDIR).unwrap();
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("assistant answer".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
}

async fn create_session(app: axum::Router) -> String {
    let response = request(
        app,
        "POST",
        "/sessions",
        Some(json!({"agent": "build", "model": "fake", "workdir": WORKDIR})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let created: CreateSessionResponse = serde_json::from_value(body_json(response).await).unwrap();
    format!("ses_{}", created.session.as_uuid().simple())
}

async fn wait_until_busy(app: axum::Router, session: &str) {
    for _ in 0..100 {
        let response = request(app.clone(), "GET", "/session/status", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        if body[session]["type"] == "busy" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("session did not become busy");
}

async fn prompt_message(app: axum::Router, session: &str) -> String {
    let response = request(
        app,
        "POST",
        &format!("/sessions/{session}/prompt"),
        Some(json!({"text": "revert me"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let prompt: PromptResponse = serde_json::from_value(body_json(response).await).unwrap();
    assert_eq!(prompt.finish, FinishReason::Stop);
    prompt.message.to_string()
}

#[tokio::test]
async fn opencode_session_revert_records_and_clears_reverted_message() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;
    let message = prompt_message(app.clone(), &session).await;

    let reverted = request(
        app.clone(),
        "POST",
        &format!("/session/{session}/revert"),
        Some(json!({"messageID": message})),
    )
    .await;
    assert_eq!(reverted.status(), StatusCode::OK);
    let reverted = body_json(reverted).await;
    assert_eq!(reverted["id"], session);
    assert_eq!(reverted["revert"]["messageID"], message);
    assert!(reverted.get("metadata").is_none());

    let unreverted = request(app, "POST", &format!("/session/{session}/unrevert"), None).await;
    assert_eq!(unreverted.status(), StatusCode::OK);
    let unreverted = body_json(unreverted).await;
    assert_eq!(unreverted["id"], session);
    assert!(unreverted.get("revert").is_none());
}

#[tokio::test]
async fn opencode_session_revert_missing_session_returns_not_found() {
    let app = router(state().await);
    let missing = SessionId::new().to_string();
    let expected = json!({
        "name": "NotFoundError",
        "data": { "message": format!("Session not found: {missing}") },
    });

    let reverted = request(
        app.clone(),
        "POST",
        &format!("/session/{missing}/revert"),
        Some(json!({"messageID": "msg_missing"})),
    )
    .await;
    assert_eq!(reverted.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(reverted).await, expected);

    let unreverted = request(app, "POST", &format!("/session/{missing}/unrevert"), None).await;
    assert_eq!(unreverted.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(unreverted).await, expected);
}

#[tokio::test]
async fn opencode_session_revert_rejects_busy_sessions() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;
    let shell_app = app.clone();
    let shell_session = session.clone();
    let mut shell_task = tokio::spawn(async move {
        request(
            shell_app,
            "POST",
            &format!("/sessions/{shell_session}/shell"),
            Some(json!({"command": "sleep 20 && printf should-not-finish"})),
        )
        .await
    });
    wait_until_busy(app.clone(), &session).await;

    let expected = json!({
        "_tag": "SessionBusyError",
        "sessionID": session,
        "message": format!("Session is busy: {session}"),
    });
    let reverted = request(
        app.clone(),
        "POST",
        &format!("/session/{session}/revert"),
        Some(json!({"messageID": "msg_missing"})),
    )
    .await;
    assert_eq!(reverted.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(reverted).await, expected);

    let unreverted = request(
        app.clone(),
        "POST",
        &format!("/session/{session}/unrevert"),
        None,
    )
    .await;
    assert_eq!(unreverted.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(unreverted).await, expected);

    let aborted = request(app, "POST", &format!("/session/{session}/abort"), None).await;
    assert_eq!(aborted.status(), StatusCode::OK);
    assert_eq!(body_json(aborted).await, json!(true));
    let shell = tokio::select! {
        joined = &mut shell_task => joined.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(3)) => {
            shell_task.abort();
            panic!("shell request did not finish after abort");
        }
    };
    assert_eq!(shell.status(), StatusCode::OK);
    let response: PromptResponse = serde_json::from_value(body_json(shell).await).unwrap();
    assert_eq!(response.finish, FinishReason::Cancelled);
}
