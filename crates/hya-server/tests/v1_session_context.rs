//! `RefreshSessionContext` (`POST /v1/sessions/{id}/refresh-context`,
//! `/reload-context`): a session records its environment, `AGENTS.md`
//! files, and skill index at its first turn; the refresh discards that
//! record (`InitialContextInvalidated`) so the next turn records the files
//! as they are then. Before a first turn there is nothing to discard.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_engine::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Answers every round with one text part and records its system prompt.
struct RecordingProvider {
    systems: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.systems
            .lock()
            .unwrap()
            .push(req.system.clone().unwrap_or_default());
        let part = PartId::new();
        let events = vec![
            Event::TextStart {
                session,
                message,
                part,
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: "done".to_string(),
            },
            Event::TextEnd {
                session,
                message,
                part,
            },
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ];
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

async fn app(systems: Arc<Mutex<Vec<String>>>) -> (axum::Router, Arc<SessionEngine>) {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider { systems })));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    ));
    let state = AppState::new(
        engine.clone(),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    );
    (router(state), engine)
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

async fn turn(app: &axum::Router, session: &str) {
    let (status, body) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({ "prompt": { "text": "hi" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let turn = body["turn"]["id"].as_str().unwrap().to_owned();
    let (status, body) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn refresh(app: &axum::Router, session: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/refresh-context"),
        json!({}),
    )
    .await
}

#[tokio::test]
async fn the_next_turn_records_the_agents_md_as_it_is_after_a_refresh() {
    let dir = support::tempdir("refresh-context");
    std::fs::write(dir.join("AGENTS.md"), "RULE_ONE_MARKER\n").unwrap();
    let systems = Arc::new(Mutex::new(Vec::new()));
    let (app, engine) = app(systems.clone()).await;
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/sessions",
        json!({ "agent": "build", "model": "fake", "workdir": dir.to_string_lossy() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let session = body["session"]["id"].as_str().unwrap().to_owned();

    // Nothing recorded before the first turn: nothing to discard.
    let (status, body) = refresh(&app, &session).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["pending"],
        Value::Null,
        "protojson omits false: {body}"
    );

    turn(&app, &session).await;
    std::fs::write(dir.join("AGENTS.md"), "RULE_TWO_MARKER\n").unwrap();
    // Without a refresh the recorded context stays.
    turn(&app, &session).await;
    let (status, body) = refresh(&app, &session).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["pending"], true, "{body}");
    turn(&app, &session).await;

    let systems = systems.lock().unwrap().clone();
    assert_eq!(systems.len(), 3, "{systems:?}");
    assert!(systems[0].contains("RULE_ONE_MARKER"), "{}", systems[0]);
    assert!(systems[1].contains("RULE_ONE_MARKER"), "{}", systems[1]);
    assert!(systems[2].contains("RULE_TWO_MARKER"), "{}", systems[2]);
    assert!(!systems[2].contains("RULE_ONE_MARKER"), "{}", systems[2]);

    let id: SessionId = session.parse().unwrap();
    let events = engine.replay(id).await.unwrap();
    let kinds = events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::InitialContextRecorded { .. } => Some("recorded"),
            Event::InitialContextInvalidated { .. } => Some("invalidated"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(kinds, ["recorded", "invalidated", "recorded"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_unknown_session_is_not_found() {
    let (app, _engine) = app(Arc::new(Mutex::new(Vec::new()))).await;
    let (status, body) = refresh(&app, &SessionId::new().to_string()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}
