#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::api::{CreateSessionResponse, PromptResponse};
use yaca_proto::{AgentName, FinishReason, ModelRef};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-session-api";

async fn state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("assistant answer".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_session(app: axum::Router) -> String {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"agent": "build", "model": "fake", "workdir": WORKDIR}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created: CreateSessionResponse = serde_json::from_value(body_json(resp).await).unwrap();
    format!("ses_{}", created.session.as_uuid().simple())
}

async fn post_prompt(app: axum::Router, session: &str) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/sessions/{session}/prompt"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"text": "hello"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let prompt: PromptResponse = serde_json::from_value(body_json(resp).await).unwrap();
    assert_eq!(prompt.finish, FinishReason::Stop);
}

#[tokio::test]
async fn opencode_session_routes_list_get_and_messages() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;
    post_prompt(app.clone(), &session).await;

    let list = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_body = body_json(list).await;
    assert_eq!(list_body[0]["id"], session);
    assert_eq!(list_body[0]["agent"], "build");
    assert_eq!(list_body[0]["model"]["id"], "fake");
    assert_eq!(list_body[0]["model"]["providerID"], "yaca");
    assert_eq!(list_body[0]["directory"], WORKDIR);
    let created = list_body[0]["time"]["created"].as_u64().expect("created");
    let updated = list_body[0]["time"]["updated"].as_u64().expect("updated");
    assert!(updated >= created);

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let get_body = body_json(get).await;
    assert_eq!(get_body["id"], session);
    assert_eq!(get_body["projectID"], "local");
    assert_eq!(get_body["version"], env!("CARGO_PKG_VERSION"));

    let messages = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/message"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(messages.status(), StatusCode::OK);
    let message_body = body_json(messages).await;
    assert_eq!(message_body[0]["info"]["sessionID"], session);
    assert_eq!(message_body[0]["info"]["role"], "user");
    assert_eq!(message_body[0]["parts"][0]["type"], "text");
    assert_eq!(message_body[0]["parts"][0]["text"], "hello");
    assert_eq!(
        message_body[0]["parts"][0]["messageID"],
        message_body[0]["info"]["id"]
    );
    assert_eq!(message_body[1]["info"]["role"], "assistant");
    assert_eq!(message_body[1]["parts"][0]["text"], "assistant answer");
}
