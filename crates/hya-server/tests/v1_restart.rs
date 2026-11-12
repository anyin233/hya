//! Restart restores use the same server admission and context as client turns.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod support;

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::stream;
use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, MessageId, ModelRef, OwnerRunId, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tower::ServiceExt;

#[derive(Default)]
struct WaitingProvider {
    requests: Mutex<Vec<CompletionRequest>>,
    entered: Notify,
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

#[tokio::test]
async fn restored_turn_has_guidance_and_rejects_a_concurrent_client_turn() {
    let workdir = std::env::temp_dir().join(format!("hya-restart-{}", SessionId::new()));
    std::fs::create_dir(&workdir).unwrap();
    std::fs::write(workdir.join("AGENTS.md"), "RESTORE_GUIDANCE_MARKER").unwrap();
    let provider = Arc::new(WaitingProvider::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            store.clone(),
            Arc::new(ProviderRouter::new().with(provider.clone())),
            support::test_runtime(Arc::new(ToolRegistry::builtins())),
            permission,
            EventBus::default(),
        )
        .with_runtime_owner(owner),
    );
    let agent = Arc::new(AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("hya/offline"),
        system_prompt: "base".into(),
        workdir: workdir.clone(),
        reasoning: None,
    });
    // One failed restore (session_busy) must not prevent the next root.
    let busy = engine
        .create(CreateSession {
            parent: None,
            project: None,
            kind: hya_proto::SessionKind::Project,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let interrupted = MessageId::new();
    store
        .append_event(
            busy,
            &hya_proto::Event::MessageStarted {
                session: busy,
                message: interrupted,
                role: hya_proto::Role::Assistant,
                agent: None,
                model: None,
            },
        )
        .await
        .unwrap();
    store
        .close_open_turns(busy, hya_proto::FinishCause::Shutdown, "stopping")
        .await
        .unwrap();
    let session = engine
        .create(CreateSession {
            parent: None,
            project: None,
            kind: hya_proto::SessionKind::Project,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let message = MessageId::new();
    store
        .append_event(
            session,
            &hya_proto::Event::MessageStarted {
                session,
                message,
                role: hya_proto::Role::Assistant,
                agent: None,
                model: None,
            },
        )
        .await
        .unwrap();
    store
        .close_open_turns(session, hya_proto::FinishCause::Shutdown, "stopping")
        .await
        .unwrap();
    let state = AppState::new(engine.clone(), agent.clone());
    let app = router(state.clone());
    // Another engine turn owns admission but has not started its message yet.
    let busy_lease = engine.try_begin_turn(busy).unwrap();
    // Simulate dying after prompt injection, before any assistant starts.
    assert!(engine.inject_restart_prompt(session).await.unwrap());
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 2);
    assert_eq!(state.resume_interrupted_turns().await, vec![session]);
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .unwrap();
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
    assert_eq!(store.list_pending_resumes().await.unwrap()[0].session, busy);
    assert!(!store.replay(busy).await.unwrap().iter().any(|e| matches!(
        e.event,
        hya_proto::Event::MessageStarted {
            role: hya_proto::Role::User,
            ..
        }
    )));
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/sessions/{session}/turns"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"prompt":{"text":"racing prompt"}}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let cancel = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/sessions/{session}/turns/{message}/cancel"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancel.status(), StatusCode::OK);
    drop(busy_lease);
    tokio::time::timeout(Duration::from_secs(2), async {
        while engine.turn_active(session) || engine.turn_active(busy) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    store.delete_session(busy).await.unwrap();
    let events = store.replay(session).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.event,
        hya_proto::Event::TextDelta { delta, .. } if delta.contains("Your previous turn was interrupted by a restart.")))
            .count(),
        1
    );
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    std::fs::remove_dir_all(workdir).unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(
        provider.requests.lock().unwrap()[0]
            .system
            .as_ref()
            .unwrap()
            .contains("RESTORE_GUIDANCE_MARKER")
    );
}
