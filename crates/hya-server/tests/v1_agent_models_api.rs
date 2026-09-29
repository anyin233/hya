//! v1 AgentModels rpc integration: durable per-agent model preferences over
//! the `/v1/agent-models` routes.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::{Arc, Mutex};

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

/// One `save_configuration` call the fake received: agent id and model (`None` clears).
type SavedConfiguration = (String, Option<AgentModelIdentity>);

#[derive(Clone, Default)]
struct FakeAgentModelControl {
    saved: Arc<Mutex<Vec<SavedConfiguration>>>,
}

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
        agent_id: String,
        model: Option<AgentModelIdentity>,
        _base_model: hya_proto::ModelRef,
    ) -> AgentModelControlFuture<'_, AgentModelState> {
        async move {
            self.saved.lock().unwrap().push((agent_id, model.clone()));
            let mut state = row(model);
            state.configuration_path = Some("/tmp/config.yaml".to_string());
            state.configuration = state.preference.clone();
            Ok(state)
        }
        .boxed()
    }

    fn list_model_effort_preferences(
        &self,
    ) -> AgentModelControlFuture<'_, Vec<hya_store::ModelEffortPreference>> {
        async move { Err(AgentModelControlError::unavailable()) }.boxed()
    }

    fn set_model_effort_preference(
        &self,
        _provider_id: String,
        _model_id: String,
        _effort: String,
    ) -> AgentModelControlFuture<'_, ()> {
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
    let control: Arc<dyn AgentModelControl> = Arc::new(FakeAgentModelControl::default());
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
    let control: Arc<dyn AgentModelControl> = Arc::new(FakeAgentModelControl::default());
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

/// `SetAgentModel` is a PUT: its `directory` and `session` scope come from
/// the JSON body (protojson mapping), never from the query string.
#[tokio::test]
async fn v1_set_agent_model_reads_its_scope_from_the_body() {
    let control: Arc<dyn AgentModelControl> = Arc::new(FakeAgentModelControl::default());
    let app = app(Some(control)).await;
    let preference = json!({"providerId": "fake", "modelId": "model"});

    let (status, body) = request(
        app.clone(),
        Method::PUT,
        "/v1/agent-models/general",
        json!({"directory": "relative/dir", "preference": preference}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("absolute path")),
        "{body}"
    );

    let (status, body) = request(
        app.clone(),
        Method::PUT,
        "/v1/agent-models/general",
        json!({"session": "not-a-session", "preference": preference}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("invalid_argument"));

    let dir = support::tempdir("agent-models-body-scope");
    let (status, body) = request(
        app.clone(),
        Method::PUT,
        "/v1/agent-models/general",
        json!({"directory": dir.to_string_lossy(), "preference": preference}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // A query `directory` is not this rpc's mapping: ignored.
    let (status, body) = request(
        app,
        Method::PUT,
        "/v1/agent-models/general?directory=relative",
        json!({"preference": preference}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn v1_saves_agent_model_configuration_and_clears_it() {
    let fake = FakeAgentModelControl::default();
    let saved = Arc::clone(&fake.saved);
    let control: Arc<dyn AgentModelControl> = Arc::new(fake);
    let app = app(Some(control)).await;

    let (status, body) = request(
        app.clone(),
        Method::PUT,
        "/v1/agent-models/general/configuration",
        json!({"model": {"providerId": "fake", "modelId": "saved"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["configurationPath"], "/tmp/config.yaml");
    assert_eq!(body["configuration"]["providerId"], "fake");
    assert_eq!(
        saved.lock().unwrap().as_slice(),
        &[(
            "general".to_string(),
            Some(AgentModelIdentity::new("fake", "saved"))
        )]
    );

    let (status, body) = request(
        app,
        Method::PUT,
        "/v1/agent-models/general/configuration",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("configuration").is_none());
    assert_eq!(
        saved.lock().unwrap().last().cloned(),
        Some(("general".to_string(), None))
    );
}
