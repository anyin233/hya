//! Thinking-effort request consistency on `/v1` turns: a session's effort is
//! resolved per model, never inherited from the startup model. With no
//! explicit choice the request omits the effort, a selected `#low`/`#none`
//! variant reaches the request verbatim, and a switched model resolves its
//! own configured default instead of carrying the startup one.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;

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
use hya_store::SessionStore;
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
        let served = match model.as_str().split_once('#') {
            Some((base, _)) => base,
            None => model.as_str(),
        };
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
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Startup agent on `fake` carrying the startup model's configured `medium`.
async fn fixture(label: &str) -> Fixture {
    let dir = support::tempdir(label).canonicalize().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (perm, _asks) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(EffortProvider {
            requests: Arc::clone(&requests),
        }))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    ));
    let state = AppState::new(
        engine,
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: dir.clone(),
            reasoning: Some(ReasoningEffort::Medium),
        }),
    );
    Fixture {
        app: router(state),
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
