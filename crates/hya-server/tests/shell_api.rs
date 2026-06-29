#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::api::{CreateSessionResponse, PromptResponse};
use hya_proto::{
    AgentName, FinishReason, ModelRef, PartProjection, Projection, Role, ToolPartState,
};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::json;
use tower::ServiceExt;

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-server-shell-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: PathBuf) -> AppState {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
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
            workdir,
            reasoning: None,
        }),
    )
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn shell_endpoint_runs_command_and_records_tool_result() {
    // Given
    let dir = tempdir();
    let app = router(state(dir.clone()).await);
    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "agent": "build",
                        "model": "fake",
                        "workdir": dir.to_string_lossy(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let created: CreateSessionResponse = serde_json::from_value(body_json(create).await).unwrap();
    let session = created.session.to_string();

    // When
    let shell = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/sessions/{session}/shell"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"command": "printf server-shell-ok"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(shell.status(), StatusCode::OK);
    let response: PromptResponse = serde_json::from_value(body_json(shell).await).unwrap();
    assert_eq!(response.finish, FinishReason::Stop);

    let events = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/sessions/{session}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let projection = Projection::from_events(
        &serde_json::from_value::<Vec<hya_proto::Envelope>>(body_json(events).await).unwrap(),
    );
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|message| message.id == response.message)
        .expect("assistant shell message");
    assert_eq!(assistant.role, Role::Assistant);
    assert!(assistant.parts.iter().any(|part| {
        matches!(
            part,
            PartProjection::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "shell" && output["output"].as_str().unwrap().contains("server-shell-ok")
        )
    }));
}
