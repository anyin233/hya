use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::stream;
use hya_engine::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{
    AgentName, Event, FinishCause, FinishReason, MessageId, ModelRef, OwnerRunId, Role, SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tower::ServiceExt;

use super::support;

#[derive(Default)]
pub(super) struct WaitingProvider {
    pub(super) requests: Mutex<Vec<CompletionRequest>>,
    pub(super) entered: Notify,
}
#[async_trait]
impl Provider for WaitingProvider {
    fn id(&self) -> &str {
        "hya"
    }
    fn capabilities(&self, _: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }
    async fn stream(
        &self,
        request: CompletionRequest,
        _: SessionId,
        _: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().unwrap().push(request);
        self.entered.notify_one();
        Ok(Box::pin(stream::pending()))
    }
}

pub(super) fn next_engine(
    store: &SessionStore,
    provider: Arc<WaitingProvider>,
    owner: OwnerRunId,
) -> SessionEngine {
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(
        store.clone(),
        Arc::new(ProviderRouter::new().with(provider)),
        support::test_runtime(Arc::new(hya_tool_loader::builtins())),
        permission,
        EventBus::default(),
    )
    .with_runtime_owner(owner)
}

pub(super) async fn fixture() -> (
    SessionStore,
    Arc<WaitingProvider>,
    Arc<SessionEngine>,
    Arc<AgentSpec>,
    SessionId,
) {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let provider = Arc::new(WaitingProvider::default());
    let engine = Arc::new(next_engine(&store, provider.clone(), owner));
    let agent = Arc::new(AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("hya/offline"),
        system_prompt: "base".into(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    });
    let session = engine
        .create(CreateSession {
            parent: None,
            project: None,
            kind: hya_proto::SessionKind::Project,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    (store, provider, engine, agent, session)
}

pub(super) async fn post_prompt(app: axum::Router, session: SessionId) -> StatusCode {
    app.oneshot(
        Request::post(format!("/v2/sessions/{session}/turns"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"prompt":{"text":"work"}}"#))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

pub(super) async fn wait_idle(app: axum::Router, session: SessionId) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/v2/sessions/{session}/turns/test"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if info["state"] != "TURN_STATE_RUNNING" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

pub(super) async fn seed_interruption(store: &SessionStore, session: SessionId) -> MessageId {
    let message = MessageId::new();
    store
        .append_event(
            session,
            &Event::MessageStarted {
                session,
                message,
                role: Role::Assistant,
                agent: None,
                model: None,
            },
        )
        .await
        .unwrap();
    store
        .close_open_turns(session, FinishCause::Shutdown, "stopping")
        .await
        .unwrap();
    message
}

pub(super) async fn restore_count(store: &SessionStore, session: SessionId) -> usize {
    store.replay(session).await.unwrap().iter().filter(|e| matches!(
        &e.event, Event::TextDelta { delta, .. } if delta.contains("Your previous turn was interrupted by a restart.")
    )).count()
}

pub(super) fn messages(
    projection: &hya_proto::Projection,
) -> Vec<(Role, Option<FinishReason>, Option<FinishCause>)> {
    projection
        .session
        .messages
        .iter()
        .map(|message| (message.role, message.finish, message.cause))
        .collect()
}

pub(super) async fn entered(notify: &Notify) {
    tokio::time::timeout(Duration::from_secs(5), notify.notified())
        .await
        .unwrap();
}
