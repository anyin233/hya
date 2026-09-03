//! Root Session model defaults from the Agent model control boundary.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::FutureExt as _;
use http_body_util::BodyExt as _;
use hya_core::{AgentSpec, EventBus, SessionEngine, TurnBinding};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{
    AgentModelControl, AgentModelControlFuture, AgentModelEffective, AgentModelIdentity,
    AgentModelSource, AgentModelState, AppState, router,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt as _;

#[derive(Clone)]
struct RememberedRootModel;

impl AgentModelControl for RememberedRootModel {
    fn available(&self) -> bool {
        true
    }

    fn list(
        &self,
        _binding: TurnBinding,
        _base_model: hya_proto::ModelRef,
    ) -> AgentModelControlFuture<'_, Vec<AgentModelState>> {
        async move {
            Ok(vec![AgentModelState {
                agent_id: "build".to_string(),
                description: None,
                mode: "primary".to_string(),
                hidden: false,
                configured: false,
                settable: true,
                preference: Some(AgentModelIdentity::new("remembered", "root/model")),
                preference_available: true,
                effective: AgentModelEffective {
                    model: AgentModelIdentity::new("remembered", "root/model"),
                    source: AgentModelSource::Remembered,
                },
            }])
        }
        .boxed()
    }

    fn set(
        &self,
        _binding: TurnBinding,
        _agent_id: String,
        _preference: Option<AgentModelIdentity>,
        _base_model: hya_proto::ModelRef,
    ) -> AgentModelControlFuture<'_, AgentModelState> {
        async move { unreachable!("Session creation never mutates Agent preferences") }.boxed()
    }
}

async fn app() -> axum::Router {
    let runtime = support::test_runtime(Arc::new(ToolRegistry::builtins()));
    let store = SessionStore::connect_memory().await.unwrap();
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        store,
        providers,
        runtime,
        permission,
        EventBus::default(),
    ));
    let agent = Arc::new(AgentSpec {
        name: "build".into(),
        model: "fallback".into(),
        system_prompt: "test".to_string(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    });
    router(AppState::new(engine, agent).with_agent_model_control(Arc::new(RememberedRootModel)))
}

async fn create(body: Value) -> (StatusCode, Value) {
    let response = app()
        .await
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/session")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn omitted_root_model_uses_the_selected_agents_effective_default() {
    let (status, body) = create(json!({ "agent": "build" })).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["agent"], "build");
    assert_eq!(body["data"]["model"]["providerID"], "remembered");
    assert_eq!(body["data"]["model"]["id"], "root/model");
}

#[tokio::test]
async fn explicit_root_session_model_stays_higher_precedence() {
    let (status, body) = create(json!({
        "agent": "build",
        "model": { "providerID": "request", "id": "explicit/model" }
    }))
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["data"]["model"]["providerID"], "request");
    assert_eq!(body["data"]["model"]["id"], "explicit/model");
}
