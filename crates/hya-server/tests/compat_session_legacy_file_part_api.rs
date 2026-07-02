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
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-compat-legacy-file-part-api";

async fn state() -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
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

#[tokio::test]
async fn legacy_message_parts_include_prompt_attachment_parts() {
    let app = router(state().await);
    let (status, created) = post_json(
        app.clone(),
        "/api/session".to_string(),
        json!({"location": {"directory": WORKDIR}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");

    let file = json!({
        "uri": "data:image/png;base64,aGVsbG8=",
        "mime": "image/png",
        "name": "pixel.png",
        "description": "tiny fixture",
        "source": {"text": "@pixel.png", "start": 0, "end": 10},
    });
    let agent = json!({
        "name": "build",
        "source": {"text": "@build", "start": 11, "end": 17},
    });
    let (status, _) = post_json(
        app.clone(),
        format!("/api/session/{session}/prompt"),
        json!({
            "prompt": {
                "text": "inspect attached context",
                "files": [file],
                "agents": [agent],
            },
            "delivery": "queue",
            "resume": false,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, messages) = get_json(app, format!("/session/{session}/message")).await;
    assert_eq!(status, StatusCode::OK);
    let user = &messages[0];
    let file_part = user["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["type"] == "file")
        .expect("file part");

    assert_eq!(file_part["sessionID"], session);
    assert_eq!(file_part["messageID"], user["info"]["id"]);
    assert_eq!(file_part["mime"], "image/png");
    assert_eq!(file_part["filename"], "pixel.png");
    assert_eq!(file_part["url"], "data:image/png;base64,aGVsbG8=");
    assert_eq!(
        file_part["source"],
        json!({
            "type": "file",
            "path": "pixel.png",
            "text": {"value": "@pixel.png", "start": 0, "end": 10}
        })
    );

    let agent_part = user["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["type"] == "agent")
        .expect("agent part");
    assert_eq!(agent_part["sessionID"], session);
    assert_eq!(agent_part["messageID"], user["info"]["id"]);
    assert_eq!(agent_part["name"], "build");
    assert_eq!(
        agent_part["source"],
        json!({"value": "@build", "start": 11, "end": 17})
    );
}

#[tokio::test]
async fn legacy_message_post_records_prompt_attachment_parts() {
    let app = router(state().await);
    let (status, created) = post_json(app.clone(), "/session".to_string(), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let session = created["id"].as_str().expect("session id");

    let file = json!({
        "type": "file",
        "url": "data:image/png;base64,aGVsbG8=",
        "mime": "image/png",
        "filename": "pixel.png",
        "source": {"text": "@pixel.png", "start": 0, "end": 10},
    });
    let agent = json!({
        "type": "agent",
        "name": "build",
        "source": {"text": "@build", "start": 11, "end": 17},
    });
    let (status, _) = post_json(
        app.clone(),
        format!("/session/{session}/message"),
        json!({"parts": [{"type": "text", "text": "inspect"}, file, agent], "noReply": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, messages) = get_json(app, format!("/session/{session}/message")).await;
    assert_eq!(status, StatusCode::OK);
    let parts = messages[0]["parts"].as_array().expect("parts");
    assert!(parts.iter().any(|part| part["type"] == "file"));
    assert!(parts.iter().any(|part| part["type"] == "agent"));
}
