//! v1 AgentModels rpc integration: durable per-agent model preferences over
//! the `/v1/agent-models` routes.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use futures::FutureExt as _;
use http_body_util::BodyExt as _;
use hya_core::{AgentSpec, EventBus, SessionEngine, TurnBinding};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{
    AGENT_MODEL_CONFIGURED, AGENT_MODEL_CONTROL_FAILURE, AGENT_MODEL_UNAVAILABLE,
    AGENT_MODEL_UNKNOWN_AGENT, AgentModelControl, AgentModelControlError, AgentModelControlFuture,
    AgentModelEffective, AgentModelIdentity, AgentModelSource, AgentModelState, AppState, router,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt as _;

#[derive(Clone)]
struct FakeAgentModelControl;

impl AgentModelControl for FakeAgentModelControl {
    fn available(&self) -> bool {
        true
    }

    fn configuration_available(&self) -> bool {
        false
    }

    fn list(
        &self,
        _binding: TurnBinding,
        _base_model: hya_proto::ModelRef,
    ) -> AgentModelControlFuture<'_, Vec<AgentModelState>> {
        async move { Ok(vec![row(Some(AgentModelIdentity::new("hya", "offline")))]) }.boxed()
    }

    fn set(
        &self,
        _binding: TurnBinding,
        agent_id: String,
        preference: Option<AgentModelIdentity>,
        _base_model: hya_proto::ModelRef,
    ) -> AgentModelControlFuture<'_, AgentModelState> {
        async move {
            match agent_id.as_str() {
                "missing" => Err(AgentModelControlError::new(
                    AGENT_MODEL_UNKNOWN_AGENT,
                    "unknown Agent `missing`",
                )),
                "configured" => Err(AgentModelControlError::new(
                    AGENT_MODEL_CONFIGURED,
                    "Agent `configured` has an explicit model policy",
                )),
                "store-failure" => Err(AgentModelControlError::new(
                    AGENT_MODEL_CONTROL_FAILURE,
                    "durable mutation failed",
                )),
                _ if preference
                    .as_ref()
                    .is_some_and(|model| model.provider_id == "missing") =>
                {
                    Err(AgentModelControlError::new(
                        AGENT_MODEL_UNAVAILABLE,
                        "model is unavailable",
                    ))
                }
                "general" => Ok(row(preference)),
                _ => unreachable!("unexpected test Agent id"),
            }
        }
        .boxed()
    }

    fn save_configuration(
        &self,
        _binding: TurnBinding,
        _agent_id: String,
        _model: Option<AgentModelIdentity>,
        _base_model: hya_proto::ModelRef,
    ) -> AgentModelControlFuture<'_, AgentModelState> {
        async move { Err(AgentModelControlError::unavailable()) }.boxed()
    }
}

fn row(preference: Option<AgentModelIdentity>) -> AgentModelState {
    let preference_available = preference.is_some();
    let effective_model = preference
        .clone()
        .unwrap_or_else(|| AgentModelIdentity::new("hya", "offline"));
    AgentModelState {
        agent_id: "general".to_string(),
        description: Some("General agent".to_string()),
        mode: "subagent".to_string(),
        hidden: false,
        configured: false,
        settable: true,
        preference: preference.clone(),
        preference_available,
        effective: AgentModelEffective {
            model: effective_model,
            source: if preference_available {
                AgentModelSource::Remembered
            } else {
                AgentModelSource::Default
            },
        },
        configuration: None,
        configuration_path: None,
        session_override: None,
    }
}

async fn app(control: Option<Arc<dyn AgentModelControl>>) -> axum::Router {
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
        model: "hya/offline".into(),
        system_prompt: "test".to_string(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    });
    let state = AppState::new(engine, agent);
    router(match control {
        Some(control) => state.with_agent_model_control(control),
        None => state,
    })
}

async fn request(
    app: axum::Router,
    method: Method,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn v1_lists_updates_and_clears_agent_model_preferences() {
    let control: Arc<dyn AgentModelControl> = Arc::new(FakeAgentModelControl);
    let (status, list) = request(
        app(Some(control.clone())).await,
        Method::GET,
        "/v1/agent-models",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["agents"][0]["agentId"], "general");
    assert_eq!(list["agents"][0]["preference"]["providerId"], "hya");
    assert_eq!(list["agents"][0]["source"], "AGENT_MODEL_SOURCE_REMEMBERED");

    let (status, updated) = request(
        app(Some(control.clone())).await,
        Method::PUT,
        "/v1/agent-models/general",
        json!({"preference": {"providerId": "fake", "modelId": "model"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["agentId"], "general");
    assert_eq!(updated["preference"]["modelId"], "model");
    assert_eq!(updated["source"], "AGENT_MODEL_SOURCE_REMEMBERED");

    // Null preference clears the remembered row.
    let (status, cleared) = request(
        app(Some(control)).await,
        Method::PUT,
        "/v1/agent-models/general",
        json!({"preference": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert!(cleared.get("preference").is_none());
    assert_eq!(cleared["source"], "AGENT_MODEL_SOURCE_DEFAULT");
}

#[tokio::test]
async fn v1_agent_model_errors_map_to_the_stable_table() {
    let control: Arc<dyn AgentModelControl> = Arc::new(FakeAgentModelControl);
    let app = app(Some(control)).await;

    let (status, body) = request(
        app.clone(),
        Method::PUT,
        "/v1/agent-models/missing",
        json!({"preference": {"providerId": "hya", "modelId": "offline"}}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], json!("not_found"));

    let (status, body) = request(
        app.clone(),
        Method::PUT,
        "/v1/agent-models/configured",
        json!({"preference": {"providerId": "hya", "modelId": "offline"}}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], json!("conflict"));

    let (status, body) = request(
        app.clone(),
        Method::PUT,
        "/v1/agent-models/store-failure",
        json!({"preference": {"providerId": "hya", "modelId": "offline"}}),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"]["code"], json!("internal"));

    let (status, body) = request(
        app.clone(),
        Method::PUT,
        "/v1/agent-models/general",
        json!({"preference": {"providerId": "missing", "modelId": "model"}}),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], json!("unavailable"));

    let (status, body) = request(
        app.clone(),
        Method::PUT,
        "/v1/agent-models/general",
        json!({"preference": {"providerId": "", "modelId": ""}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("invalid_argument"));
}

#[tokio::test]
async fn v1_agent_models_unavailable_without_installed_control() {
    let (status, body) = request(
        app(None).await,
        Method::GET,
        "/v1/agent-models",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], json!("unavailable"));
}
