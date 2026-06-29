#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-session-v2-context-state-api";

async fn shell_state() -> AppState {
    std::fs::create_dir_all(WORKDIR).unwrap();
    let provider = FakeProvider::scripted(vec![]);
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn get_json(app: axum::Router, uri: String) -> (StatusCode, Value) {
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
    (status, body_json(resp).await)
}

async fn post_json(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn patch_json(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

fn tool_state<'a>(context: &'a Value, status: &str) -> Option<&'a Value> {
    context["data"]
        .as_array()?
        .iter()
        .flat_map(|message| {
            message["content"]
                .as_array()
                .into_iter()
                .flat_map(|content| content.iter())
        })
        .find_map(|content| (content["state"]["status"] == status).then_some(&content["state"]))
}

#[tokio::test]
async fn opencode_v2_context_error_tool_state_includes_result() {
    let app = router(shell_state().await);
    let (status, created) = post_json(
        app.clone(),
        "/api/session".to_string(),
        json!({"location": {"directory": WORKDIR}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");

    let (status, shell) = post_json(
        app.clone(),
        format!("/api/session/{session}/shell"),
        json!({"agent": "build", "command": "printf original"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let message = shell["data"]["info"]["id"].as_str().expect("message id");
    let part = shell["data"]["parts"][0]["id"].as_str().expect("part id");
    let call = shell["data"]["parts"][0]["callID"]
        .as_str()
        .expect("call id");

    let (status, _) = patch_json(
        app.clone(),
        format!("/session/{session}/message/{message}/part/{part}"),
        json!({
            "id": part,
            "sessionID": session,
            "messageID": message,
            "type": "tool",
            "callID": call,
            "tool": "shell",
            "state": {
                "status": "error",
                "input": {"command": "printf original"},
                "error": "shell failed",
                "time": {"start": 1, "end": 2}
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, context) = get_json(app, format!("/api/session/{session}/context")).await;
    assert_eq!(status, StatusCode::OK);
    let state = tool_state(&context, "error").expect("error tool state");
    assert_eq!(state["result"], "shell failed");
}

#[tokio::test]
async fn opencode_v2_context_pending_tool_state_keeps_string_input() {
    let app = router(shell_state().await);
    let (status, created) = post_json(
        app.clone(),
        "/api/session".to_string(),
        json!({"location": {"directory": WORKDIR}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");

    let (status, shell) = post_json(
        app.clone(),
        format!("/api/session/{session}/shell"),
        json!({"agent": "build", "command": "printf pending"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let message = shell["data"]["info"]["id"].as_str().expect("message id");
    let part = shell["data"]["parts"][0]["id"].as_str().expect("part id");
    let call = shell["data"]["parts"][0]["callID"]
        .as_str()
        .expect("call id");

    let (status, _) = patch_json(
        app.clone(),
        format!("/session/{session}/message/{message}/part/{part}"),
        json!({
            "id": part,
            "sessionID": session,
            "messageID": message,
            "type": "tool",
            "callID": call,
            "tool": "shell",
            "state": {"status": "pending", "input": ""}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, context) = get_json(app, format!("/api/session/{session}/context")).await;
    assert_eq!(status, StatusCode::OK);
    let state = tool_state(&context, "pending").expect("pending tool state");
    assert_eq!(state["input"], "");
}
