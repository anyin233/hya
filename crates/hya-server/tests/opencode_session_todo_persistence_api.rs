#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::api::CreateSessionResponse;
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-session-todo-persistence-api";

fn temp_db() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!(
            "hya-session-todo-{nanos}-{}.db",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

fn expected_todos() -> Value {
    json!([
        { "content": "Audit OpenCode todos", "status": "in_progress", "priority": "high" },
        { "content": "Document remaining gaps", "status": "pending", "priority": "medium" }
    ])
}

async fn state_at(path: &str, with_todowrite: bool) -> AppState {
    let provider = if with_todowrite {
        FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "todowrite".to_string(),
                    input: json!({ "todos": expected_todos() }),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("todos updated".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ])
    } else {
        FakeProvider::scripted(vec![])
    };
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::TodoWrite,
        "*",
        Mode::Allow,
    )]));
    let store = SessionStore::connect(path).await.unwrap();
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
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_session(app: axum::Router) -> String {
    let body = json!({"agent": "build", "model": "fake", "workdir": WORKDIR});
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created: CreateSessionResponse = serde_json::from_value(body_json(resp).await).unwrap();
    created.session.to_string()
}

async fn post_prompt(app: axum::Router, session: &str) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/sessions/{session}/prompt"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"text": "update todos"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

async fn get_todo(app: axum::Router, session: &str) -> Value {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/todo"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await
}

#[tokio::test]
async fn opencode_session_todo_persists_across_restart() {
    let db = temp_db();
    let app = router(state_at(&db, true).await);
    let session = create_session(app.clone()).await;
    post_prompt(app.clone(), &session).await;
    assert_eq!(get_todo(app, &session).await, expected_todos());

    let reopened = router(state_at(&db, false).await);
    assert_eq!(get_todo(reopened, &session).await, expected_todos());
}
