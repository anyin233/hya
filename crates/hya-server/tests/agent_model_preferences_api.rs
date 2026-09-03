//! Dedicated TUI Agent model preference routes and bootstrap capability.

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
    AGENT_MODEL_CONFIGURED, AGENT_MODEL_CONTROL_FAILURE, AGENT_MODEL_CONTROL_UNAVAILABLE,
    AGENT_MODEL_UNAVAILABLE, AGENT_MODEL_UNKNOWN_AGENT, AgentModelControl, AgentModelControlError,
    AgentModelControlFuture, AgentModelEffective, AgentModelIdentity, AgentModelSource,
    AgentModelState, AppState, router,
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
        preference,
        preference_available,
        effective: AgentModelEffective {
            model: effective_model,
            source: if preference_available {
                AgentModelSource::Remembered
            } else {
                AgentModelSource::Default
            },
        },
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
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .header(
                    "x-opencode-directory",
                    std::env::temp_dir().to_string_lossy().as_ref(),
                )
                .body(Body::from(body.to_string()))
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
async fn installed_control_lists_updates_and_advertises_agent_model_preferences() {
    let control: Arc<dyn AgentModelControl> = Arc::new(FakeAgentModelControl);
    let (status, list) = request(
        app(Some(control.clone())).await,
        Method::GET,
        "/tui/agent-models",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["agentID"], "general");
    assert_eq!(list[0]["preference"]["providerID"], "hya");
    assert_eq!(list[0]["effective"]["source"], "remembered");

    let (status, updated) = request(
        app(Some(control.clone())).await,
        Method::PUT,
        "/tui/agent-models/general",
        json!({"preference": {"providerID": "hya", "modelID": "family/revision"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["agentID"], "general");
    assert_eq!(updated["preference"]["modelID"], "family/revision");

    let (status, cleared) = request(
        app(Some(control.clone())).await,
        Method::PUT,
        "/tui/agent-models/general",
        json!({"preference": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(cleared["preference"].is_null());
    assert_eq!(cleared["effective"]["source"], "default");

    let (status, bootstrap) = request(
        app(Some(control)).await,
        Method::GET,
        "/tui/bootstrap",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bootstrap["capabilities"]["agentModelPreferences"], true);
    assert_eq!(bootstrap["agentModels"][0]["agentID"], "general");
}

#[tokio::test]
async fn mutation_errors_use_strict_statuses_and_structured_bodies() {
    let cases = [
        (
            "/tui/agent-models/missing",
            json!({"preference": null}),
            StatusCode::NOT_FOUND,
            AGENT_MODEL_UNKNOWN_AGENT,
        ),
        (
            "/tui/agent-models/configured",
            json!({"preference": {"providerID": "hya", "modelID": "offline"}}),
            StatusCode::CONFLICT,
            AGENT_MODEL_CONFIGURED,
        ),
        (
            "/tui/agent-models/general",
            json!({"preference": {"providerID": "missing", "modelID": "model"}}),
            StatusCode::BAD_REQUEST,
            AGENT_MODEL_UNAVAILABLE,
        ),
        (
            "/tui/agent-models/store-failure",
            json!({"preference": null}),
            StatusCode::SERVICE_UNAVAILABLE,
            AGENT_MODEL_CONTROL_FAILURE,
        ),
    ];
    for (path, payload, expected_status, expected_code) in cases {
        let (status, body) = request(
            app(Some(Arc::new(FakeAgentModelControl))).await,
            Method::PUT,
            path,
            payload,
        )
        .await;
        assert_eq!(status, expected_status, "body: {body}");
        assert_eq!(body["error"]["code"], expected_code);
        assert!(body["error"]["message"].is_string());
    }

    let (status, body) = request(
        app(Some(Arc::new(FakeAgentModelControl))).await,
        Method::PUT,
        "/tui/agent-models/general",
        json!({"preference": null, "unexpected": true}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["error"]["code"], "AGENT_MODEL_INVALID_REQUEST");
}
#[tokio::test]
async fn absent_control_is_not_advertised_and_rejects_agent_model_routes() {
    let (status, body) = request(
        app(None).await,
        Method::GET,
        "/tui/agent-models",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
    assert_eq!(body["error"]["code"], AGENT_MODEL_CONTROL_UNAVAILABLE);

    let (status, bootstrap) =
        request(app(None).await, Method::GET, "/tui/bootstrap", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bootstrap["capabilities"]["agentModelPreferences"], false);
    assert_eq!(bootstrap["agentModels"], json!([]));
}
