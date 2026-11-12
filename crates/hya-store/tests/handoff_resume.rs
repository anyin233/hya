//! Restore prompts are consumed atomically with their transcript append.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::{AgentName, Event, ModelRef, OwnerRunId, Role, SessionId};
use hya_store::SessionStore;

#[tokio::test]
async fn restore_is_appended_once_and_survives_a_new_owner() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let session = SessionId::new();
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
    store
        .save_pending_interaction(&hya_store::PendingInteraction::new(
            "unanswered",
            Some(session),
            "permission",
            "{}",
        ))
        .await
        .unwrap();
    store
        .queue_restart_resume(session, owner, "restore: bash did not finish")
        .await
        .unwrap();
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
    assert!(store.list_pending_interactions().await.unwrap().is_empty());
    store.release_runtime_owner(owner).unwrap();
    let next = OwnerRunId::new();
    store.claim_runtime_owner(next).unwrap();
    let envelopes = store.take_restart_resume(session, next).await.unwrap();
    assert!(envelopes.iter().any(|e| matches!(&e.event, Event::TextDelta { delta, .. } if delta == "restore: bash did not finish")));
    assert!(
        store
            .take_restart_resume(session, next)
            .await
            .unwrap()
            .is_empty()
    );
    let projection = store.read_projection(session).await.unwrap();
    assert_eq!(projection.session.messages.len(), 1);
    assert_eq!(projection.session.messages[0].role, Role::User);
    assert!(store.list_pending_resumes().await.unwrap().is_empty());
    store
        .queue_restart_resume(session, next, "second stop")
        .await
        .unwrap();
    assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
}
