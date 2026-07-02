#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-compat-legacy-tool-attachment-api";

async fn state() -> AppState {
    std::fs::create_dir_all(WORKDIR).unwrap();
    std::fs::write(
        format!("{WORKDIR}/image.png"),
        [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
    )
    .unwrap();
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({ "filePath": "image.png" }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![FakeStep::Finish(FinishReason::Stop)],
    ]);
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, permission, EventBus::default());
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

async fn request_json(app: axum::Router, method: &str, uri: String, body: Option<Value>) -> Value {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn legacy_tool_state_includes_file_attachments() {
    let app = router(state().await);
    let created = request_json(
        app.clone(),
        "POST",
        "/api/session".to_string(),
        Some(json!({"location": {"directory": WORKDIR}})),
    )
    .await;
    let session = created["data"]["id"].as_str().unwrap();

    request_json(
        app.clone(),
        "POST",
        format!("/sessions/{session}/prompt"),
        Some(json!({"text": "read the image"})),
    )
    .await;
    let messages = request_json(app, "GET", format!("/session/{session}/message"), None).await;
    let tool = messages[1]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["type"] == "tool" && part["tool"] == "read")
        .expect("read tool part");
    let attachment = &tool["state"]["attachments"][0];

    assert_eq!(attachment["sessionID"], session);
    assert_eq!(attachment["messageID"], messages[1]["info"]["id"]);
    assert_eq!(attachment["type"], "file");
    assert_eq!(attachment["mime"], "image/png");
    assert_eq!(attachment["url"], "data:image/png;base64,iVBORw0KGgo=");
}
