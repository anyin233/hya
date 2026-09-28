//! Durable pending-interaction reply persistence tests.

use hya_proto::SessionId;
use hya_store::{PendingInteraction, PendingInteractionReply, SessionStore};

#[tokio::test]
async fn orphaned_interaction_reply_survives_and_is_acknowledged_once()
-> Result<(), hya_store::StoreError> {
    let store = SessionStore::connect_memory().await?;
    let session = SessionId::new();
    store
        .save_pending_interaction(&PendingInteraction::new(
            "perm_handoff_test",
            Some(session),
            "permission",
            "{}",
        ))
        .await?;
    store
        .queue_pending_interaction_reply(&PendingInteractionReply {
            id: "perm_handoff_test".to_string(),
            kind: "permission".to_string(),
            payload: r#"{"reply":"once"}"#.to_string(),
            created_at: 1,
        })
        .await?;
    let replies = store.list_pending_interaction_replies().await?;
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].id, "perm_handoff_test");
    assert!(
        store
            .claim_pending_interaction_reply(&replies[0].id)
            .await?
    );
    assert!(
        !store
            .claim_pending_interaction_reply(&replies[0].id)
            .await?
    );
    assert!(store.list_pending_interaction_replies().await?.is_empty());
    assert!(store.list_pending_interactions().await?.is_empty());
    Ok(())
}
