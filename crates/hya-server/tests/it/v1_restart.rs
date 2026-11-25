//! Restart restores use the same server admission and context as client turns.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "v1_restart/fixture.rs"]
mod fixture;

use self::fixture::*;
use crate::support;
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use hya_engine::{
    BoundSidecarFactory, CoreError, SessionEngine, SidecarEnvironment, SidecarHandle, SidecarStart,
    TurnBinding,
};
use hya_proto::{FinishCause, FinishReason, OwnerRunId, Role};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tower::ServiceExt;

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
