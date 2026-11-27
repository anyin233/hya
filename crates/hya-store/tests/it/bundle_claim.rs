//! Integration tests for generic `bundle_claim` atomic acquire/renew/release/expire/takeover.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_store::{BundleClaim, SessionStore, StoreError};
use serde_json::json;

struct TempDb(String);

impl TempDb {
    fn new(tag: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        Self(
            std::env::temp_dir()
                .join(format!(
                    "hya-bundle-claim-{tag}-{nanos}-{}-{id}.db",
                    std::process::id()
                ))
                .to_string_lossy()
                .into_owned(),
        )
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", ".runtime-owner.lock"] {
            let _ = std::fs::remove_file(format!("{}{}", self.0, suffix));
        }
    }
}

#[tokio::test]
async fn bundle_claim_acquire_renew_release_and_epoch_advance() {
    let store = SessionStore::connect_memory().await.unwrap();

    // Acquire new lease with epoch = 1.
    let first = store
        .acquire_bundle_claim_at(
            "hya/team",
            "resident:main/worker",
            "run-1",
            Some(5_000),
            Some(&json!({"slot": 1})),
            1_000,
        )
        .await
        .unwrap();
    assert_eq!(first.bundle, "hya/team");
    assert_eq!(first.claim_key, "resident:main/worker");
    assert_eq!(first.owner, "run-1");
    assert_eq!(first.epoch, 1);
    assert_eq!(first.expires_at, Some(6_000));
    assert_eq!(first.metadata, json!({"slot": 1}));
    assert_eq!(first.acquired_at, 1_000);
    assert_eq!(first.updated_at, 1_000);

    // Same or different owner cannot re-acquire while claim is active & unexpired.
    let err = store
        .acquire_bundle_claim_at(
            "hya/team",
            "resident:main/worker",
            "run-2",
            None,
            Some(&json!({})),
            2_000,
        )
        .await
        .expect_err("active claim must conflict");
    assert!(matches!(err, StoreError::BundleClaimConflict { .. }));

    // Another bundle can claim the same claim_key independently.
    let other_bundle = store
        .acquire_bundle_claim_at(
            "hya/workflow",
            "resident:main/worker",
            "run-2",
            None,
            Some(&json!({"ok": true})),
            2_000,
        )
        .await
        .unwrap();
    assert_eq!(other_bundle.epoch, 1);

    // Renew with matching owner/epoch updates expiry & metadata.
    let renewed = store
        .renew_bundle_claim_at(
            &first,
            Some(10_000),
            Some(&json!({"slot": 1, "beat": 2})),
            3_000,
        )
        .await
        .unwrap();
    assert_eq!(renewed.epoch, 1);
    assert_eq!(renewed.expires_at, Some(13_000));
    assert_eq!(renewed.metadata, json!({"slot": 1, "beat": 2}));
    assert_eq!(renewed.updated_at, 3_000);

    // Fence & renew reject wrong owner or wrong epoch.
    let wrong_owner = BundleClaim {
        owner: "run-wrong".into(),
        ..renewed.clone()
    };
    assert!(matches!(
        store
            .renew_bundle_claim_at(&wrong_owner, None, None, 3_500)
            .await
            .unwrap_err(),
        StoreError::StaleBundleClaim { .. }
    ));
    store
        .validate_bundle_claim_at(&renewed, 3_500)
        .await
        .unwrap();
    let wrong_epoch = BundleClaim {
        epoch: 2,
        ..renewed.clone()
    };
    assert!(matches!(
        store
            .validate_bundle_claim_at(&wrong_epoch, 3_500)
            .await
            .unwrap_err(),
        StoreError::StaleBundleClaim { .. }
    ));

    // Release clears active visibility and is idempotent for matching owner+epoch.
    store
        .release_bundle_claim_at(&renewed, 4_000)
        .await
        .unwrap();
    store
        .release_bundle_claim_at(&renewed, 4_100)
        .await
        .unwrap();
    assert_eq!(
        store
            .get_bundle_claim_at("hya/team", "resident:main/worker", 4_200)
            .await
            .unwrap(),
        None
    );

    // Re-acquire bumps epoch to 2; old released claim can no longer release cleanly.
    let reacquired = store
        .acquire_bundle_claim_at(
            "hya/team",
            "resident:main/worker",
            "run-2",
            None,
            Some(&json!({"slot": 2})),
            4_500,
        )
        .await
        .unwrap();
    assert_eq!(reacquired.owner, "run-2");
    assert_eq!(reacquired.epoch, 2);
    assert!(matches!(
        store
            .release_bundle_claim_at(&renewed, 4_600)
            .await
            .unwrap_err(),
        StoreError::StaleBundleClaim { .. }
    ));
}

#[tokio::test]
async fn bundle_claim_expiry_and_expire_sweep() {
    let store = SessionStore::connect_memory().await.unwrap();

    let short = store
        .acquire_bundle_claim_at(
            "hya/team",
            "lease-short",
            "run-1",
            Some(500),
            None,
            1_000,
        )
        .await
        .unwrap();
    store
        .acquire_bundle_claim_at("hya/team", "lease-forever", "run-1", None, None, 1_000)
        .await
        .unwrap();

    // At t = 1499: short lease still active; list sees both.
    assert_eq!(
        store
            .list_active_bundle_claims_at("hya/team", 1_499)
            .await
            .unwrap()
            .len(),
        2
    );

    // At t = 1500: short lease is expired (`expires_at <= now`).
    assert_eq!(
        store
            .get_bundle_claim_at("hya/team", "lease-short", 1_500)
            .await
            .unwrap(),
        None
    );
    assert!(matches!(
        store
            .validate_bundle_claim_at(&short, 1_500)
            .await
            .unwrap_err(),
        StoreError::StaleBundleClaim { .. }
    ));
    assert!(matches!(
        store
            .renew_bundle_claim_at(&short, Some(500), None, 1_500)
            .await
            .unwrap_err(),
        StoreError::StaleBundleClaim { .. }
    ));

    // A new owner can acquire the expired lease directly, bumping epoch to 2.
    let reacquired = store
        .acquire_bundle_claim_at(
            "hya/team",
            "lease-short",
            "run-2",
            Some(1_000),
            Some(&json!({"takeover": "expired"})),
            1_500,
        )
        .await
        .unwrap();
    assert_eq!(reacquired.owner, "run-2");
    assert_eq!(reacquired.epoch, 2);

    // Expire sweep transitions expired active claims to released.
    let expired_count = store
        .expire_bundle_claims_at(Some("hya/team"), 2_500)
        .await
        .unwrap();
    assert_eq!(expired_count, 1);
    let active = store
        .list_active_bundle_claims_at("hya/team", 2_500)
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].claim_key, "lease-forever");
}

#[tokio::test]
async fn bundle_claim_concurrent_acquire_allows_exactly_one_winner() {
    let db = TempDb::new("concurrent");
    let store = Arc::new(SessionStore::connect(&db.0).await.unwrap());

    let mut handles = Vec::new();
    for idx in 0..8 {
        let store = Arc::clone(&store);
        handles.push(tokio::spawn(async move {
            store
                .acquire_bundle_claim_at(
                    "hya/workflow",
                    "singleton",
                    &format!("owner-{idx}"),
                    Some(60_000),
                    Some(&json!({"idx": idx})),
                    1_000,
                )
                .await
        }));
    }

    let mut winners = Vec::new();
    let mut conflicts = 0;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(claim) => winners.push(claim),
            Err(StoreError::BundleClaimConflict { .. }) => conflicts += 1,
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
    assert_eq!(winners.len(), 1);
    assert_eq!(conflicts, 7);
    assert_eq!(winners[0].epoch, 1);
}

#[tokio::test]
async fn bundle_claim_takeover_requires_matching_runtime_owner_and_fences_prior_epoch() {
    let db = TempDb::new("takeover");
    let store = SessionStore::connect(&db.0).await.unwrap();

    // Initial active claim from an old crashed owner.
    let old = store
        .acquire_bundle_claim_at(
            "hya/team",
            "resident:lead",
            "old-run",
            None,
            Some(&json!({"gen": 1})),
            1_000,
        )
        .await
        .unwrap();
    assert_eq!(old.epoch, 1);

    // Without matching runtime owner claim, takeover is rejected.
    let unowned = store
        .takeover_bundle_claim(
            "hya/team",
            "resident:lead",
            "new-run",
            hya_proto::OwnerRunId::new(),
            None,
            Some(&json!({"gen": 2})),
        )
        .await
        .expect_err("takeover requires matching runtime owner claim");
    assert!(matches!(unowned, StoreError::RuntimeOwnerClaimRequired));

    let owner = hya_proto::OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let (taken, prev_epoch) = store
        .takeover_bundle_claim(
            "hya/team",
            "resident:lead",
            "new-run",
            owner,
            None,
            Some(&json!({"gen": 2})),
        )
        .await
        .unwrap();
    assert_eq!(prev_epoch, 1);
    assert_eq!(taken.owner, "new-run");
    assert_eq!(taken.epoch, 2);

    // Old owner can no longer pass fence inside a transaction.
    let mut tx = store.begin_transaction().await.unwrap();
    let stale = SessionStore::fence_bundle_claim_in_tx_at(&mut tx, &old, hya_proto::now_millis())
        .await
        .expect_err("old epoch must be fenced");
    assert!(matches!(stale, StoreError::StaleBundleClaim { .. }));
    SessionStore::fence_bundle_claim_in_tx_at(&mut tx, &taken, hya_proto::now_millis())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}
