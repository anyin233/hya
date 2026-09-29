//! Per-Agent default thinking effort persistence.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_store::{OwnerRunId, SessionStore, StoreError};

#[tokio::test]
async fn agent_effort_preferences_upsert_clear_and_require_the_owner() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();

    store
        .upsert_agent_effort_preference(owner, "scout", "low", 1)
        .await
        .unwrap();
    store
        .upsert_agent_effort_preference(owner, "general", "high", 2)
        .await
        .unwrap();
    store
        .upsert_agent_effort_preference(owner, "scout", "medium", 3)
        .await
        .unwrap();

    let rows: Vec<_> = store
        .list_agent_effort_preferences()
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.agent_id, row.effort))
        .collect();
    assert_eq!(
        rows,
        [
            ("general".to_string(), "high".to_string()),
            ("scout".to_string(), "medium".to_string())
        ]
    );

    store
        .clear_agent_effort_preference(owner, "scout")
        .await
        .unwrap();
    assert!(
        store
            .get_agent_effort_preference("scout")
            .await
            .unwrap()
            .is_none()
    );
    // Clearing an absent row is idempotent.
    store
        .clear_agent_effort_preference(owner, "scout")
        .await
        .unwrap();

    let stranger = OwnerRunId::new();
    let error = store
        .upsert_agent_effort_preference(stranger, "general", "low", 4)
        .await
        .unwrap_err();
    assert!(!matches!(error, StoreError::InvalidPreferenceData { .. }));
    assert_eq!(
        store
            .get_agent_effort_preference("general")
            .await
            .unwrap()
            .unwrap()
            .effort,
        "high",
        "a non-owner write must not land"
    );
}
