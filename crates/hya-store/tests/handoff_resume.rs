//! Shutdown cancellation is atomic; restore prompts survive until turn start.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::{AgentName, Event, MessageId, ModelRef, OwnerRunId, Role, SessionId};
use hya_store::SessionStore;

async fn create(store: &SessionStore, session: SessionId) {
    store
        .append_event(
            session,
            &Event::SessionCreated {
                session,
                parent: None,
                project: None,
                kind: hya_proto::SessionKind::Project,
                agent: AgentName::new("test"),
                model: ModelRef::new("test/model"),
                workdir: "/tmp".into(),
            },
        )
        .await
        .unwrap();
}

async fn open_turn(store: &SessionStore, session: SessionId) -> MessageId {
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
    message
}

#[tokio::test]
async fn restore_is_appended_once_and_survives_a_new_owner() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
    create(&store, session).await;
    let interrupted = open_turn(&store, session).await;
    store
        .cancel_turn_for_restart(session, interrupted, owner, "restore: bash did not finish")
        .await
        .unwrap();
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
    store.release_runtime_owner(owner).unwrap();
    let next = OwnerRunId::new();
    store.claim_runtime_owner(next).unwrap();
    let envelopes = store
        .take_restart_resume(session, next)
        .await
        .unwrap()
        .unwrap();
    assert!(envelopes.iter().any(|e| matches!(&e.event, Event::TextDelta { delta, .. } if delta == "restore: bash did not finish")));
    assert_eq!(
        store.list_pending_resumes().await.unwrap().len(),
        1,
        "a restore must remain pending until its assistant turn starts"
    );
    assert!(
        store
            .take_restart_resume(session, next)
            .await
            .unwrap()
            .unwrap()
            .is_empty()
    );
    let projection = store.read_projection(session).await.unwrap();
    assert_eq!(projection.session.messages.len(), 2);
    assert_eq!(projection.session.messages[1].role, Role::User);
    let restored = open_turn(&store, session).await;
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    store
        .cancel_turn_for_restart(session, restored, next, "second stop")
        .await
        .unwrap();
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
}

#[tokio::test]
async fn deleting_a_session_removes_its_restore() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
    create(&store, session).await;
    let message = open_turn(&store, session).await;
    store
        .cancel_turn_for_restart(session, message, owner, "restore")
        .await
        .unwrap();
    store.delete_session(session).await.unwrap();
    assert!(
        store
            .take_restart_resume(session, owner)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!store.session_exists(session).await.unwrap());
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
}

#[tokio::test]
async fn shutdown_cancellation_and_restore_are_one_transaction() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
    create(&store, session).await;
    let message = open_turn(&store, session).await;
    store
        .cancel_turn_for_restart(session, message, owner, "stopping")
        .await
        .unwrap();
    assert_eq!(
        store.list_pending_resumes().await.unwrap().len(),
        1,
        "the transaction that cancels a root must also leave a restore row"
    );
}

#[tokio::test]
async fn archived_root_is_not_restored_at_stop_or_start() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
    create(&store, session).await;
    let first = open_turn(&store, session).await;
    store
        .cancel_turn_for_restart(session, first, owner, "restore")
        .await
        .unwrap();
    store
        .append_event(
            session,
            &Event::SessionArchived {
                session,
                archived: 123.into(),
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .take_restart_resume(session, owner)
            .await
            .unwrap()
            .is_none()
    );
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    let second = open_turn(&store, session).await;
    store
        .cancel_turn_for_restart(session, second, owner, "restore")
        .await
        .unwrap();
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
}

#[tokio::test]
async fn another_cancel_replaces_an_unconsumed_restore() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
    create(&store, session).await;
    for prompt in ["first stop", "second stop"] {
        let message = open_turn(&store, session).await;
        store
            .cancel_turn_for_restart(session, message, owner, prompt)
            .await
            .unwrap();
    }
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
    let injected = store
        .take_restart_resume(session, owner)
        .await
        .unwrap()
        .unwrap();
    assert!(injected.iter().any(|entry| matches!(&entry.event,
        Event::TextDelta { delta, .. } if delta == "second stop")));
}
