//! Restart restores use the same server admission and context as client turns.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod support;

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::stream;
use hya_core::{
    AgentSpec, BoundSidecarFactory, CoreError, CreateSession, EventBus, SessionEngine,
    SidecarEnvironment, SidecarHandle, SidecarStart, TurnBinding,
};
use hya_proto::{
    AgentName, Event, FinishCause, FinishReason, MessageId, ModelRef, OwnerRunId, Role, SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Barrier, Notify};
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

fn next_engine(
    store: &SessionStore,
    provider: Arc<WaitingProvider>,
    owner: OwnerRunId,
) -> SessionEngine {
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    SessionEngine::new(
        store.clone(),
        Arc::new(ProviderRouter::new().with(provider)),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    )
    .with_runtime_owner(owner)
}

async fn fixture() -> (
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

async fn post_prompt(app: axum::Router, session: SessionId) -> StatusCode {
    app.oneshot(
        Request::post(format!("/v1/sessions/{session}/turns"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"prompt":{"text":"work"}}"#))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

async fn wait_idle(app: axum::Router, session: SessionId) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/v1/sessions/{session}/turns/test"))
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

async fn seed_interruption(store: &SessionStore, session: SessionId) -> MessageId {
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

async fn restore_count(store: &SessionStore, session: SessionId) -> usize {
    store.replay(session).await.unwrap().iter().filter(|e| matches!(
        &e.event, Event::TextDelta { delta, .. } if delta.contains("Your previous turn was interrupted by a restart.")
    )).count()
}

#[tokio::test]
async fn shutdown_between_admission_and_spawn_closes_the_admitted_prompt() {
    let (store, provider, engine, agent, session) = fixture().await;
    let barrier = Arc::new(Barrier::new(2));
    let state = AppState::new(engine.clone(), agent.clone())
        .with_turn_spawn_barrier(session, barrier.clone());
    let app = router(state);
    let request = tokio::spawn(post_prompt(app.clone(), session));
    barrier.wait().await;
    assert!(!engine.turn_active(session));
    engine
        .stop_for_restart(Duration::from_secs(1))
        .await
        .unwrap();
    barrier.wait().await;
    assert_eq!(request.await.unwrap(), StatusCode::OK);
    wait_idle(app, session).await;
    let projection = store.read_projection(session).await.unwrap();
    let last = projection.session.messages.last().unwrap();
    assert_eq!(
        (last.role, last.finish, last.cause),
        (
            Role::Assistant,
            Some(FinishReason::Cancelled),
            Some(FinishCause::Shutdown)
        )
    );
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
    let next = AppState::new(
        Arc::new(next_engine(
            &store,
            provider.clone(),
            engine.runtime_owner(),
        )),
        agent,
    );
    assert_eq!(next.resume_interrupted_turns().await, vec![session]);
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .unwrap();
    assert_eq!(restore_count(&store, session).await, 1);
    next.engine
        .stop_for_restart(Duration::from_secs(1))
        .await
        .unwrap();
    wait_idle(router(next), session).await;
}

#[tokio::test]
async fn second_shutdown_before_restore_spawn_gets_one_new_prompt() {
    let (store, provider, engine, agent, session) = fixture().await;
    let original = seed_interruption(&store, session).await;
    let barrier = Arc::new(Barrier::new(2));
    let state = AppState::new(engine.clone(), agent.clone())
        .with_turn_spawn_barrier(session, barrier.clone());
    let app = router(state.clone());
    let restore = tokio::spawn(async move { state.resume_interrupted_turns().await });
    barrier.wait().await;
    assert_eq!(restore_count(&store, session).await, 1);
    engine
        .stop_for_restart(Duration::from_secs(1))
        .await
        .unwrap();
    barrier.wait().await;
    assert_eq!(restore.await.unwrap(), vec![session]);
    wait_idle(app, session).await;
    let rows = store.list_pending_resumes().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(
        rows[0].message, original,
        "a second stop is a new interruption"
    );
    let next = AppState::new(
        Arc::new(next_engine(
            &store,
            provider.clone(),
            engine.runtime_owner(),
        )),
        agent,
    );
    assert_eq!(next.resume_interrupted_turns().await, vec![session]);
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .unwrap();
    assert_eq!(restore_count(&store, session).await, 2);
    next.engine
        .stop_for_restart(Duration::from_secs(1))
        .await
        .unwrap();
    wait_idle(router(next), session).await;
}

#[derive(Clone, Default)]
struct StartingSidecar(Arc<Notify>);

impl SidecarEnvironment for StartingSidecar {
    fn factory_for(
        &self,
        _: &TurnBinding,
        _: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        Ok(Some(Arc::new(self.clone())))
    }
}

#[async_trait]
impl BoundSidecarFactory for StartingSidecar {
    async fn start(&self, _: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait]
impl SidecarHandle for StartingSidecar {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.0.notify_one();
        std::future::pending().await
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
}

#[tokio::test]
async fn user_cancel_before_restored_assistant_start_clears_the_restore() {
    let (store, provider, engine, agent, session) = fixture().await;
    seed_interruption(&store, session).await;
    let sidecar = Arc::new(StartingSidecar::default());
    let engine = Arc::new(
        next_engine(&store, provider.clone(), engine.runtime_owner())
            .with_sidecar_environment(sidecar.clone()),
    );
    let state = AppState::new(engine.clone(), agent.clone());
    let app = router(state.clone());
    assert_eq!(state.resume_interrupted_turns().await, vec![session]);
    tokio::time::timeout(Duration::from_secs(5), sidecar.0.notified())
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/sessions/{session}/turns/test/cancel"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    wait_idle(app, session).await;
    let projection = store.read_projection(session).await.unwrap();
    let last = projection.session.messages.last().unwrap();
    assert_eq!(
        (last.role, last.finish, last.cause),
        (
            Role::Assistant,
            Some(FinishReason::Cancelled),
            Some(FinishCause::UserCancel)
        )
    );
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    let next = AppState::new(
        Arc::new(next_engine(
            &store,
            provider.clone(),
            engine.runtime_owner(),
        )),
        agent,
    );
    assert!(next.resume_interrupted_turns().await.is_empty());
    assert_eq!(restore_count(&store, session).await, 1);
    assert!(provider.requests.lock().unwrap().is_empty());
}

struct FailingSidecar;

impl SidecarEnvironment for FailingSidecar {
    fn factory_for(
        &self,
        _: &TurnBinding,
        _: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        Err(CoreError::Invalid("sidecar preparation failed".into()))
    }
}

#[tokio::test]
async fn failure_before_restored_assistant_start_closes_the_prompt_with_its_error() {
    let (store, provider, engine, agent, session) = fixture().await;
    seed_interruption(&store, session).await;
    let engine = Arc::new(
        next_engine(&store, provider.clone(), engine.runtime_owner())
            .with_sidecar_environment(Arc::new(FailingSidecar)),
    );
    let state = AppState::new(engine, agent);
    assert_eq!(state.resume_interrupted_turns().await, vec![session]);
    wait_idle(router(state.clone()), session).await;
    let projection = store.read_projection(session).await.unwrap();
    let last = projection.session.messages.last().unwrap();
    assert_eq!(
        (last.role, last.finish, last.cause),
        (Role::Assistant, Some(FinishReason::Error), None)
    );
    assert!(
        last.error
            .as_ref()
            .unwrap()
            .message
            .contains("sidecar preparation failed")
    );
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    assert!(state.resume_interrupted_turns().await.is_empty());
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reverting_past_interrupted_message_drops_its_restore() {
    for marked in [false, true] {
        let (store, provider, engine, agent, session) = fixture().await;
        let user = engine
            .admit_user_prompt(session, "work".into())
            .await
            .unwrap();
        seed_interruption(&store, session).await;
        if marked {
            assert!(engine.inject_restart_prompt(session).await.unwrap());
        }
        store
            .append_event(
                session,
                &Event::SessionReverted {
                    session,
                    message: user,
                    files: Vec::new(),
                },
            )
            .await
            .unwrap();
        let state = AppState::new(engine, agent);
        assert!(state.resume_interrupted_turns().await.is_empty());
        assert!(store.list_pending_resumes().await.unwrap().is_empty());
        assert!(
            store
                .read_projection(session)
                .await
                .unwrap()
                .session
                .revert
                .is_some()
        );
        assert!(provider.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn server_stop_waits_for_an_admitted_run_before_spawn() {
    let (store, _, engine, agent, session) = fixture().await;
    let barrier = Arc::new(Barrier::new(2));
    let state = AppState::new(engine, agent).with_turn_spawn_barrier(session, barrier.clone());
    let app = router(state.clone());
    let request = tokio::spawn(post_prompt(app.clone(), session));
    barrier.wait().await;
    let mut stop =
        tokio::spawn(async move { state.stop_for_restart(Duration::from_secs(2)).await });
    let returned_early = tokio::time::timeout(Duration::from_millis(50), &mut stop).await;
    barrier.wait().await;
    assert_eq!(request.await.unwrap(), StatusCode::OK);
    wait_idle(app, session).await;
    assert!(
        returned_early.is_err(),
        "stop returned while its admitted server run was still held"
    );
    stop.await.unwrap().unwrap();
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
}

#[tokio::test]
async fn cancel_before_spawn_keeps_user_cause_even_if_shutdown_follows() {
    for shutdown in [false, true] {
        let (store, _, engine, agent, session) = fixture().await;
        let barrier = Arc::new(Barrier::new(2));
        let state =
            AppState::new(engine.clone(), agent).with_turn_spawn_barrier(session, barrier.clone());
        let app = router(state);
        let request = tokio::spawn(post_prompt(app.clone(), session));
        barrier.wait().await;
        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/v1/sessions/{session}/turns/test/cancel"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        if shutdown {
            engine
                .stop_for_restart(Duration::from_secs(1))
                .await
                .unwrap();
        }
        barrier.wait().await;
        assert_eq!(request.await.unwrap(), StatusCode::OK);
        wait_idle(app, session).await;
        let projection = store.read_projection(session).await.unwrap();
        let last = projection.session.messages.last().unwrap();
        assert_eq!(
            (last.role, last.finish, last.cause),
            (
                Role::Assistant,
                Some(FinishReason::Cancelled),
                Some(FinishCause::UserCancel)
            )
        );
        assert!(store.list_pending_resumes().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn server_run_wait_is_bounded_by_the_drain_deadline() {
    let (store, _, engine, agent, session) = fixture().await;
    let barrier = Arc::new(Barrier::new(2));
    let state = AppState::new(engine, agent).with_turn_spawn_barrier(session, barrier.clone());
    let app = router(state.clone());
    let request = tokio::spawn(post_prompt(app.clone(), session));
    barrier.wait().await;
    tokio::time::timeout(
        Duration::from_secs(1),
        state.stop_for_restart(Duration::from_millis(30)),
    )
    .await
    .unwrap()
    .unwrap();
    barrier.wait().await;
    assert_eq!(request.await.unwrap(), StatusCode::OK);
    wait_idle(app.clone(), session).await;
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
    assert_eq!(post_prompt(app, session).await, StatusCode::CONFLICT);
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
    let barrier = Arc::new(Barrier::new(2));
    let state =
        AppState::new(engine.clone(), agent.clone()).with_turn_spawn_barrier(busy, barrier.clone());
    let app = router(state.clone());
    // A real client run owns admission while both restore rows are still pending.
    let busy_request = tokio::spawn(post_prompt(app.clone(), busy));
    barrier.wait().await;
    // Simulate dying after prompt injection, before any assistant starts.
    assert!(engine.inject_restart_prompt(session).await.unwrap());
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 2);
    assert_eq!(state.resume_interrupted_turns().await, vec![session]);
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .unwrap();
    let rows = store.list_pending_resumes().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session, busy);
    assert_eq!(restore_count(&store, busy).await, 0);
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
    let cancel_busy = app
        .clone()
        .oneshot(
            Request::post(format!("/v1/sessions/{busy}/turns/test/cancel"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancel_busy.status(), StatusCode::OK);
    barrier.wait().await;
    assert_eq!(busy_request.await.unwrap(), StatusCode::OK);
    wait_idle(app.clone(), busy).await;
    wait_idle(app, session).await;
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
