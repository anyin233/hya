//! Restart regressions that pause turn admission at the spawn boundary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../tests/it/v1_restart/fixture.rs"]
mod fixture;

use self::fixture::*;
use crate::{AppState, router, test_support as support};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use hya_engine::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{
    AgentName, FinishCause, FinishReason, MessageId, ModelRef, OwnerRunId, Role, SessionId,
};
use hya_provider::ProviderRouter;
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Barrier;
use tower::ServiceExt;

#[tokio::test]
async fn server_state_is_built_once_per_configuration() {
    let (_, _, engine, agent, _) = fixture().await;
    let app = AppState::new(engine, agent);
    let _router = router(app.clone());
    let _grpc = crate::V1Grpc::new(app.clone());
    let _server = crate::build(app.clone());
    let _again = router(app.clone());
    assert_eq!(app.server_builds(), 1);

    let other = app.with_auto_title(false);
    let _other = router(other.clone());
    assert_eq!(other.server_builds(), 2);
}

#[tokio::test]
async fn stop_while_a_run_is_reserved_waits_for_it_and_nothing_is_admitted() {
    let (store, provider, engine, agent, session) = fixture().await;
    let barrier = Arc::new(Barrier::new(2));
    let state = AppState::new(engine, agent).with_turn_spawn_barrier(session, barrier.clone());
    let app = router(state.clone());
    let request = tokio::spawn(post_prompt(app.clone(), session));
    barrier.wait().await;
    assert!(
        !state.engine.turn_active(session),
        "admission has no engine lease yet"
    );
    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/v1/sessions/{session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info["busy"], true, "reserved admission must remain busy");
    assert_eq!(
        post_prompt(app.clone(), session).await,
        StatusCode::CONFLICT
    );
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
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
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
