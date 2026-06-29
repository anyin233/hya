#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, MessageId, ModelRef, Role, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-session-v2-context-time-api";

struct PendingProvider;

#[async_trait]
impl Provider for PendingProvider {
    fn id(&self) -> &str {
        "pending"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        _session: SessionId,
        _message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        std::future::pending::<Result<EventStream, ProviderError>>().await
    }
}

async fn pending_state() -> hya_server::AppState {
    std::fs::create_dir_all(WORKDIR).unwrap();
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(PendingProvider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    hya_server::AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("pending"),
            system_prompt: "x".to_string(),
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn get_json(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn post_json(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

#[tokio::test]
async fn opencode_v2_context_omits_unfinished_assistant_completed_time() {
    let state = pending_state().await;
    let mut events = state.engine.bus().subscribe();
    let app = hya_server::router(state);
    let (status, created) = post_json(
        app.clone(),
        "/api/session".to_string(),
        json!({"location": {"directory": WORKDIR}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");

    let (status, _) = post_json(
        app.clone(),
        format!("/api/session/{session}/prompt"),
        json!({"prompt": {"text": "wait"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    await_assistant_started(&mut events).await;

    let (status, context) = get_json(app, format!("/api/session/{session}/context")).await;
    assert_eq!(status, StatusCode::OK);
    let assistant = context["data"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["type"] == "assistant")
        .expect("assistant message");
    assert!(assistant["time"].get("completed").is_none());
}

async fn await_assistant_started(
    events: &mut tokio::sync::broadcast::Receiver<hya_proto::Envelope>,
) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let env = events.recv().await.expect("event");
            if matches!(
                env.event,
                Event::MessageStarted {
                    role: Role::Assistant,
                    ..
                }
            ) {
                break;
            }
        }
    })
    .await
    .expect("assistant started");
}
