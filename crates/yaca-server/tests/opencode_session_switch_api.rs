#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::api::CreateSessionResponse;
use yaca_proto::{AgentName, ModelRef, SessionId};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-session-switch-api";

async fn state() -> AppState {
    let router =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(vec![]))));
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

async fn post_json(app: axum::Router, uri: String, body: Value) -> StatusCode {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

#[tokio::test]
async fn opencode_v2_session_switch_routes_update_selected_agent_and_model() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    let agent_status = post_json(
        app.clone(),
        format!("/api/session/{session}/agent"),
        json!({"agent": "plan"}),
    )
    .await;
    assert_eq!(agent_status, StatusCode::NO_CONTENT);

    let model_status = post_json(
        app.clone(),
        format!("/api/session/{session}/model"),
        json!({"model": {"providerID": "anthropic", "id": "claude-sonnet", "variant": "fast"}}),
    )
    .await;
    assert_eq!(model_status, StatusCode::NO_CONTENT);

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
    let body = body_json(get).await;
    assert_eq!(body["agent"], "plan");
    assert_eq!(body["model"]["providerID"], "anthropic");
    assert_eq!(body["model"]["id"], "claude-sonnet");

    let missing = format!("ses_{}", SessionId::new().as_uuid().simple());
    let missing_status = post_json(
        app,
        format!("/api/session/{missing}/agent"),
        json!({"agent": "plan"}),
    )
    .await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
}
