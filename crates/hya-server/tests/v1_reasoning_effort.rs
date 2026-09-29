//! Thinking-effort request consistency on `/v1` turns: a session's effort is
//! resolved per model, never inherited from the startup model. With no
//! explicit choice the request omits the effort, a selected `#low`/`#none`
//! variant reaches the request verbatim, and a switched model resolves its
//! own configured default instead of carrying the startup one.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, MessageId, ModelRef, Role, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
    ReasoningEffort,
};
use hya_server::{AppState, router};
use hya_store::{OwnerRunId, SessionStore};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Records every completion request; replies with a finished assistant message.
///
/// Simulates configured per-model defaults: `fake` (the startup model) is
/// configured with `medium`, `third` with `low`, `other` with none.
struct EffortProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait::async_trait]
impl Provider for EffortProvider {
    fn id(&self) -> &str {
        "effort"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            reasoning_request: true,
            ..Capabilities::default()
        })
    }

    fn reasoning_default(&self, model: &ModelRef) -> Option<ReasoningEffort> {
        let base = match model.as_str().split_once('#') {
            Some((base, _)) => base,
            None => model.as_str(),
        };
        let served = base.strip_prefix("effort/").unwrap_or(base);
        match served {
            "fake" => Some(ReasoningEffort::Medium),
            "third" => Some(ReasoningEffort::Low),
            _ => None,
        }
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(req);
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        )])))
    }
}

struct Fixture {
    app: axum::Router,
    store: SessionStore,
    engine: Arc<SessionEngine>,
    owner: OwnerRunId,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Startup agent on `fake` with no Agent effort, as the runtime builds it:
/// model defaults come from the route per request.
async fn fixture(label: &str) -> Fixture {
    fixture_with_global(label, None).await
}

async fn fixture_with_global(label: &str, global: Option<ReasoningEffort>) -> Fixture {
    let dir = support::tempdir(label).canonicalize().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (perm, _asks) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let engine = Arc::new(
        SessionEngine::new(
            store.clone(),
            Arc::new(ProviderRouter::new().with(Arc::new(EffortProvider {
                requests: Arc::clone(&requests),
            }))),
            support::test_runtime(Arc::new(ToolRegistry::builtins())),
            perm,
            EventBus::default(),
        )
        .with_global_reasoning(global),
    );
    let state = AppState::new(
        Arc::clone(&engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: dir.clone(),
            reasoning: None,
        }),
    );
    Fixture {
        app: router(state),
        store,
        engine,
        owner,
        requests,
        dir,
    }
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

impl Fixture {
    async fn session(&self, model: &str) -> String {
        let (status, created) = call(
            &self.app,
            Method::POST,
            "/v1/sessions",
            json!({"agent": "build", "model": model, "workdir": self.dir.to_string_lossy()}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        created["session"]["id"].as_str().unwrap().to_owned()
    }

    async fn switch(&self, session: &str, model: &str) {
        let (status, updated) = call(
            &self.app,
            Method::PATCH,
            &format!("/v1/sessions/{session}"),
            json!({"model": model}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");
    }

    /// Run one prompt turn and return the request it produced.
    async fn prompt_request(&self, session: &str, index: usize) -> CompletionRequest {
        let (status, created) = call(
            &self.app,
            Method::POST,
            &format!("/v1/sessions/{session}/turns"),
            json!({"prompt": {"text": "record effort"}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        let turn = created["turn"]["id"].as_str().unwrap().to_owned();
        let (status, waited) = call(
            &self.app,
            Method::POST,
            &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{waited}");
        assert_eq!(waited["state"], json!("TURN_STATE_FINISHED"), "{waited}");
        self.requests.lock().unwrap()[index].clone()
    }
}

#[tokio::test]
async fn session_on_the_startup_model_keeps_its_configured_default() {
    let fx = fixture("effort-startup").await;
    let session = fx.session("fake").await;

    let request = fx.prompt_request(&session, 0).await;
    assert_eq!(request.model, ModelRef::new("fake"));
    assert_eq!(request.reasoning, Some(ReasoningEffort::Medium));
}

#[tokio::test]
async fn no_choice_on_an_unconfigured_model_omits_the_effort() {
    let fx = fixture("effort-no-choice").await;
    // Created away from the startup model: the startup `medium` must not leak.
    let session = fx.session("other").await;

    let request = fx.prompt_request(&session, 0).await;
    assert_eq!(request.model, ModelRef::new("other"));
    assert_eq!(request.reasoning, None);
}

#[tokio::test]
async fn switching_models_resolves_each_model_s_own_default() {
    let fx = fixture("effort-switch").await;
    let session = fx.session("fake").await;
    fx.prompt_request(&session, 0).await;

    fx.switch(&session, "third").await;
    let request = fx.prompt_request(&session, 1).await;
    assert_eq!(request.model, ModelRef::new("third"));
    assert_eq!(request.reasoning, Some(ReasoningEffort::Low));

    // The switched model without a configured default sends nothing; the
    // startup `medium` and the previous `low` are both gone.
    fx.switch(&session, "other").await;
    let request = fx.prompt_request(&session, 2).await;
    assert_eq!(request.model, ModelRef::new("other"));
    assert_eq!(request.reasoning, None);
}

#[tokio::test]
async fn selected_low_and_none_variants_reach_the_request() {
    let fx = fixture("effort-variants").await;
    let session = fx.session("fake").await;

    fx.switch(&session, "other#low").await;
    let request = fx.prompt_request(&session, 0).await;
    assert_eq!(request.reasoning, Some(ReasoningEffort::Low));

    // `#none` is an explicit choice and survives the route's reasoning
    // support, unlike an absent effort.
    fx.switch(&session, "fake#none").await;
    let request = fx.prompt_request(&session, 1).await;
    assert_eq!(request.reasoning, Some(ReasoningEffort::Off));

    fx.switch(&session, "other#none").await;
    let request = fx.prompt_request(&session, 2).await;
    assert_eq!(request.reasoning, Some(ReasoningEffort::Off));
}

#[tokio::test]
async fn invalid_variant_sends_no_effort_instead_of_a_default() {
    let fx = fixture("effort-invalid-variant").await;
    let session = fx.session("fake").await;

    fx.switch(&session, "fake#bogus").await;
    let request = fx.prompt_request(&session, 0).await;
    assert_eq!(request.reasoning, None);

    fx.switch(&session, "third#bogus").await;
    let request = fx.prompt_request(&session, 1).await;
    assert_eq!(request.reasoning, None);
}

impl Fixture {
    async fn prefer(&self, provider: &str, model: &str, effort: Option<&str>) {
        match effort {
            Some(effort) => self
                .store
                .upsert_model_effort_preference(self.owner, provider, model, effort, 1)
                .await
                .unwrap(),
            None => self
                .store
                .clear_model_effort_preference(self.owner, provider, model)
                .await
                .unwrap(),
        }
    }

    /// `(effectiveEffort, effortSource)` the session info reports.
    async fn reported(&self, session: &str) -> (String, String) {
        let (status, info) = call(
            &self.app,
            Method::GET,
            &format!("/v1/sessions/{session}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{info}");
        (
            info["effectiveEffort"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            info["effortSource"].as_str().unwrap_or_default().to_owned(),
        )
    }
}

/// A stored per-model preference applies to the very next request (no
/// restart), loses to an explicit `#variant`, and clearing it falls back.
/// Session info reports exactly what the request carries.
#[tokio::test]
async fn stored_preference_drives_the_next_request_and_session_info() {
    let fx = fixture("effort-preference").await;
    let session = fx.session("effort/other").await;
    assert_eq!(
        fx.reported(&session).await,
        (String::new(), "EFFORT_SOURCE_NONE".to_owned())
    );

    fx.prefer("effort", "other", Some("high")).await;
    let request = fx.prompt_request(&session, 0).await;
    assert_eq!(request.reasoning, Some(ReasoningEffort::High));
    assert_eq!(
        fx.reported(&session).await,
        ("high".to_owned(), "EFFORT_SOURCE_PREFERENCE".to_owned())
    );

    fx.switch(&session, "effort/other#low").await;
    let request = fx.prompt_request(&session, 1).await;
    assert_eq!(request.reasoning, Some(ReasoningEffort::Low));
    assert_eq!(
        fx.reported(&session).await,
        ("low".to_owned(), "EFFORT_SOURCE_SUFFIX".to_owned())
    );

    fx.switch(&session, "effort/other").await;
    fx.prefer("effort", "other", None).await;
    let request = fx.prompt_request(&session, 2).await;
    assert_eq!(request.reasoning, None);
    assert_eq!(
        fx.reported(&session).await,
        (String::new(), "EFFORT_SOURCE_NONE".to_owned())
    );
}

/// Precedence below the preference: the model's configured default, then
/// the global `reasoning:` default.
#[tokio::test]
async fn model_default_beats_global_default_which_beats_nothing() {
    let fx = fixture_with_global("effort-global", Some(ReasoningEffort::High)).await;
    let session = fx.session("third").await;
    let request = fx.prompt_request(&session, 0).await;
    assert_eq!(request.reasoning, Some(ReasoningEffort::Low));
    assert_eq!(
        fx.reported(&session).await,
        ("low".to_owned(), "EFFORT_SOURCE_MODEL_DEFAULT".to_owned())
    );

    fx.switch(&session, "other").await;
    let request = fx.prompt_request(&session, 1).await;
    assert_eq!(request.reasoning, Some(ReasoningEffort::High));
    assert_eq!(
        fx.reported(&session).await,
        ("high".to_owned(), "EFFORT_SOURCE_GLOBAL_DEFAULT".to_owned())
    );
}

/// A preference outranks the model's configured default.
#[tokio::test]
async fn preference_outranks_the_model_default() {
    let fx = fixture("effort-pref-over-default").await;
    // `effort/third` is `third`, whose configured default is `low`.
    let session = fx.session("effort/third").await;
    fx.prefer("effort", "third", Some("high")).await;
    let request = fx.prompt_request(&session, 0).await;
    assert_eq!(request.reasoning, Some(ReasoningEffort::High));
    assert_eq!(
        fx.reported(&session).await,
        ("high".to_owned(), "EFFORT_SOURCE_PREFERENCE".to_owned())
    );
}

/// The Agent's own default effort (layer 2): the user's runtime choice beats
/// `agents.<id>.reasoning`, both beat the per-model preference, and a model
/// `#suffix` still beats them all. Session info reports the Agent layer.
#[tokio::test]
async fn agent_effort_layers_between_suffix_and_model_preference() {
    let fx = fixture("effort-agent").await;
    let session = fx.session("effort/other").await;
    fx.prefer("effort", "other", Some("medium")).await;

    fx.engine
        .runtime_registry()
        .publish_agent_effort_configuration(
            [("build".to_string(), ReasoningEffort::Low)]
                .into_iter()
                .collect(),
        );
    let request = fx.prompt_request(&session, 0).await;
    assert_eq!(
        request.reasoning,
        Some(ReasoningEffort::Low),
        "configured beats model pref"
    );
    assert_eq!(
        fx.reported(&session).await,
        ("low".to_owned(), "EFFORT_SOURCE_AGENT".to_owned())
    );

    fx.store
        .upsert_agent_effort_preference(fx.owner, "build", "high", 1)
        .await
        .unwrap();
    let request = fx.prompt_request(&session, 1).await;
    assert_eq!(
        request.reasoning,
        Some(ReasoningEffort::High),
        "runtime beats configured"
    );

    fx.switch(&session, "effort/other#minimal").await;
    let request = fx.prompt_request(&session, 2).await;
    assert_eq!(
        request.reasoning,
        Some(ReasoningEffort::Minimal),
        "suffix beats agent"
    );

    fx.switch(&session, "effort/other").await;
    fx.store
        .clear_agent_effort_preference(fx.owner, "build")
        .await
        .unwrap();
    fx.engine
        .runtime_registry()
        .publish_agent_effort_configuration(Default::default());
    let request = fx.prompt_request(&session, 3).await;
    assert_eq!(
        request.reasoning,
        Some(ReasoningEffort::Medium),
        "back to model pref"
    );
}
