#![allow(clippy::unwrap_used)]

use hya_proto::SessionId;
use hya_store::{OwnerRunId, SessionStore, StoreError};

#[tokio::test]
async fn concurrent_claims_allow_exactly_one_owner() {
    let store = SessionStore::connect_memory().await.unwrap();
    let actor_id = SessionId::new();
    let owner_a = OwnerRunId::new();
    let owner_b = OwnerRunId::new();

    let (left, right) = tokio::join!(
        store.try_claim_new(actor_id, owner_a),
        store.try_claim_new(actor_id, owner_b),
    );

    let outcomes = [left, right];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(
                result,
                Err(StoreError::ActorAlreadyClaimed { actor_id: claimed })
                    if *claimed == actor_id
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn restart_recovery_increments_epoch_and_invalidates_old_claim() {
    let store = SessionStore::connect_memory().await.unwrap();
    let actor_id = SessionId::new();
    let old = store
        .try_claim_new(actor_id, OwnerRunId::new())
        .await
        .unwrap();

    let recovered = store
        .recover_claim(actor_id, OwnerRunId::new())
        .await
        .unwrap();

    assert_eq!(recovered.previous_epoch, old.epoch);
    assert_eq!(recovered.claim.epoch.get(), old.epoch.get() + 1);
    assert!(store.validate_actor_claim(&recovered.claim).await.is_ok());
    assert!(matches!(
        store.validate_actor_claim(&old).await,
        Err(StoreError::StaleActorClaim { actor_id: stale }) if stale == actor_id
    ));
}

#[tokio::test]
async fn release_requires_full_tuple_and_is_idempotent() {
    let store = SessionStore::connect_memory().await.unwrap();
    let actor_id = SessionId::new();
    let first = store
        .try_claim_new(actor_id, OwnerRunId::new())
        .await
        .unwrap();

    store.release_claim(&first).await.unwrap();
    store.release_claim(&first).await.unwrap();
    let second = store
        .try_claim_new(actor_id, OwnerRunId::new())
        .await
        .unwrap();

    assert_eq!(second.epoch.get(), first.epoch.get() + 1);
    assert!(matches!(
        store.release_claim(&first).await,
        Err(StoreError::StaleActorClaim { actor_id: stale }) if stale == actor_id
    ));
    assert!(store.validate_actor_claim(&second).await.is_ok());
}
