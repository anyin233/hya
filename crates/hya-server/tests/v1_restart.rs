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

fn messages(
    projection: &hya_proto::Projection,
) -> Vec<(Role, Option<FinishReason>, Option<FinishCause>)> {
    projection
        .session
        .messages
        .iter()
        .map(|message| (message.role, message.finish, message.cause))
        .collect()
}

#[tokio::test]
async fn stop_while_a_run_is_reserved_waits_for_it_and_nothing_is_admitted() {
    let (store, provider, engine, agent, session) = fixture().await;
    let barrier = Arc::new(Barrier::new(2));
    let state = AppState::new(engine, agent).with_turn_spawn_barrier(session, barrier.clone());
    let app = router(state.clone());
    let request = tokio::spawn(post_prompt(app.clone(), session));
    barrier.wait().await;
    let mut stop =
        tokio::spawn(async move { state.stop_for_restart(Duration::from_secs(2)).await });
    let returned_early = tokio::time::timeout(Duration::from_millis(50), &mut stop).await;
    assert!(
        returned_early.is_err(),
        "stop returned while a reserved server run was still held"
    );
    barrier.wait().await;
    assert_eq!(request.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);
    tokio::time::timeout(Duration::from_secs(1), stop)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(messages(&store.read_projection(session).await.unwrap()).is_empty());
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    assert!(provider.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn stop_returns_at_its_deadline_and_a_late_run_admits_nothing() {
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
    assert_eq!(request.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(messages(&store.read_projection(session).await.unwrap()).is_empty());
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    assert_eq!(post_prompt(app, session).await, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_dropped_handler_leaves_no_unanswered_prompt() {
    let (store, _, engine, agent, session) = fixture().await;
    let barrier = Arc::new(Barrier::new(2));
    let state = AppState::new(engine, agent).with_turn_spawn_barrier(session, barrier.clone());
    let request = tokio::spawn(post_prompt(router(state), session));
    // The handler holds its reservation here; the client goes away.
    barrier.wait().await;
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let messages = messages(&store.read_projection(session).await.unwrap());
    assert!(
        matches!(messages.last(), None | Some((Role::Assistant, Some(_), _))),
        "an admitted prompt without its answer: {messages:?}"
    );
}

#[derive(Clone, Default)]
struct GatedSidecar {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl SidecarEnvironment for GatedSidecar {
    fn factory_for(
        &self,
        _: &TurnBinding,
        _: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        Ok(Some(Arc::new(self.clone())))
    }
}

#[async_trait]
impl BoundSidecarFactory for GatedSidecar {
    async fn start(&self, _: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait]
impl SidecarHandle for GatedSidecar {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
}

fn gated_engine(
    store: &SessionStore,
    provider: &Arc<WaitingProvider>,
    owner: OwnerRunId,
) -> (Arc<SessionEngine>, Arc<GatedSidecar>) {
    let sidecar = Arc::new(GatedSidecar::default());
    let engine = Arc::new(
        next_engine(store, provider.clone(), owner).with_sidecar_environment(sidecar.clone()),
    );
    (engine, sidecar)
}

async fn entered(notify: &Notify) {
    tokio::time::timeout(Duration::from_secs(5), notify.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn stop_after_admission_closes_the_prompt_with_one_shutdown_pair() {
    let (store, provider, engine, agent, session) = fixture().await;
    let (engine, sidecar) = gated_engine(&store, &provider, engine.runtime_owner());
    let state = AppState::new(engine.clone(), agent.clone());
    let app = router(state.clone());
    assert_eq!(post_prompt(app.clone(), session).await, StatusCode::OK);
    entered(&sidecar.entered).await;
    tokio::time::timeout(
        Duration::from_secs(2),
        state.stop_for_restart(Duration::from_secs(1)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        messages(&store.read_projection(session).await.unwrap()),
        vec![
            (Role::User, Some(FinishReason::Stop), None),
            (
                Role::Assistant,
                Some(FinishReason::Cancelled),
                Some(FinishCause::Shutdown)
            ),
        ]
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
    entered(&provider.entered).await;
    assert_eq!(restore_count(&store, session).await, 1);
    next.stop_for_restart(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn stop_before_a_restore_injects_keeps_the_original_interruption() {
    let (store, provider, engine, agent, session) = fixture().await;
    let original = seed_interruption(&store, session).await;
    let barrier = Arc::new(Barrier::new(2));
    let state = AppState::new(engine.clone(), agent.clone())
        .with_turn_spawn_barrier(session, barrier.clone());
    let restore = tokio::spawn({
        let state = state.clone();
        async move { state.resume_interrupted_turns().await }
    });
    barrier.wait().await;
    let stop = tokio::spawn(async move { state.stop_for_restart(Duration::from_secs(1)).await });
    barrier.wait().await;
    assert!(restore.await.unwrap().is_empty());
    stop.await.unwrap().unwrap();
    assert_eq!(restore_count(&store, session).await, 0);
    let rows = store.list_pending_resumes().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].message, original);
    let next = AppState::new(
        Arc::new(next_engine(
            &store,
            provider.clone(),
            engine.runtime_owner(),
        )),
        agent,
    );
    assert_eq!(next.resume_interrupted_turns().await, vec![session]);
    entered(&provider.entered).await;
    assert_eq!(restore_count(&store, session).await, 1);
    next.stop_for_restart(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn a_restored_turn_stopped_before_its_message_is_a_new_interruption() {
    let (store, provider, engine, agent, session) = fixture().await;
    let original = seed_interruption(&store, session).await;
    let (engine, sidecar) = gated_engine(&store, &provider, engine.runtime_owner());
    let state = AppState::new(engine.clone(), agent.clone());
    assert_eq!(state.resume_interrupted_turns().await, vec![session]);
    entered(&sidecar.entered).await;
    assert_eq!(restore_count(&store, session).await, 1);
    state
        .stop_for_restart(Duration::from_secs(1))
        .await
        .unwrap();
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
    entered(&provider.entered).await;
    assert_eq!(restore_count(&store, session).await, 2);
    next.stop_for_restart(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn a_cancelled_second_caller_does_not_answer_a_prompt_another_turn_owns() {
    let (store, provider, engine, agent, session) = fixture().await;
    let (engine, sidecar) = gated_engine(&store, &provider, engine.runtime_owner());
    let app = router(AppState::new(engine.clone(), agent.clone()));
    assert_eq!(post_prompt(app.clone(), session).await, StatusCode::OK);
    entered(&sidecar.entered).await;
    // A lease-less caller queues behind the turn that owns the prompt.
    let cancel = tokio_util::sync::CancellationToken::new();
    let second = tokio::spawn({
        let (engine, agent, cancel) = (engine.clone(), agent.clone(), cancel.clone());
        async move { engine.run_turn(session, &agent, cancel).await }
    });
    tokio::task::yield_now().await;
    cancel.cancel();
    assert_eq!(second.await.unwrap().unwrap(), FinishReason::Cancelled);
    sidecar.release.notify_one();
    entered(&provider.entered).await;
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
    assert_eq!(
        messages(&store.read_projection(session).await.unwrap()),
        vec![
            (Role::User, Some(FinishReason::Stop), None),
            (
                Role::Assistant,
                Some(FinishReason::Cancelled),
                Some(FinishCause::UserCancel)
            ),
        ]
    );
}

#[tokio::test]
async fn user_cancel_before_restored_assistant_start_clears_the_restore() {
    let (store, provider, engine, agent, session) = fixture().await;
    seed_interruption(&store, session).await;
    let (engine, sidecar) = gated_engine(&store, &provider, engine.runtime_owner());
    let state = AppState::new(engine.clone(), agent.clone());
    let app = router(state.clone());
    assert_eq!(state.resume_interrupted_turns().await, vec![session]);
    entered(&sidecar.entered).await;
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

/// `/undo` (`LastUserMessage`) hides the injected restore prompt when the
/// previous process died after injecting it, or the interrupted turn itself
/// when nothing was injected yet: either way the next start runs nothing and
/// the pending revert stays.
#[tokio::test]
async fn undo_after_an_interruption_drops_its_restore() {
    for injected in [false, true] {
        let (store, provider, engine, agent, session) = fixture().await;
        engine
            .admit_user_prompt(session, "work".into())
            .await
            .unwrap();
        seed_interruption(&store, session).await;
        if injected {
            assert!(
                engine
                    .inject_restart_prompt(session)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        let response = router(AppState::new(engine.clone(), agent.clone()))
            .oneshot(
                Request::post(format!("/v1/sessions/{session}/revert"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let next = AppState::new(
            Arc::new(next_engine(
                &store,
                provider.clone(),
                engine.runtime_owner(),
            )),
            agent,
        );
        assert!(next.resume_interrupted_turns().await.is_empty());
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
            // The engine drain alone: the server's own stop refuses the
            // admission instead (`stop_while_a_run_is_reserved_...`).
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
    assert!(
        engine
            .inject_restart_prompt(session)
            .await
            .unwrap()
            .is_some()
    );
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
