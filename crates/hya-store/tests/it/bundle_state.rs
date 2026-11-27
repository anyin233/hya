//! Integration tests for generic `bundle_state` KV CRUD, CAS, prefix queries, and transactions.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::{Event, SessionId};
use hya_store::{SessionStore, StoreError, append_event_in_transaction};
use serde_json::json;

#[tokio::test]
async fn bundle_state_crud_and_bundle_isolation() {
    let store = SessionStore::connect_memory().await.unwrap();

    assert_eq!(store.get_bundle_state("hya/team", "k1").await.unwrap(), None);

    store
        .set_bundle_state("hya/team", "k1", b"v1")
        .await
        .unwrap();
    store
        .set_bundle_state("hya/workflow", "k1", b"wf1")
        .await
        .unwrap();

    assert_eq!(
        store.get_bundle_state("hya/team", "k1").await.unwrap(),
        Some(b"v1".to_vec())
    );
    assert_eq!(
        store.get_bundle_state("hya/workflow", "k1").await.unwrap(),
        Some(b"wf1".to_vec())
    );

    let entry = store
        .get_bundle_state_entry("hya/team", "k1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entry.bundle, "hya/team");
    assert_eq!(entry.key, "k1");
    assert_eq!(entry.value, b"v1");
    assert!(entry.updated_at > 0);

    // Overwrite updates value without affecting other bundles.
    store
        .set_bundle_state("hya/team", "k1", b"v2")
        .await
        .unwrap();
    assert_eq!(
        store.get_bundle_state("hya/team", "k1").await.unwrap(),
        Some(b"v2".to_vec())
    );
    assert_eq!(
        store.get_bundle_state("hya/workflow", "k1").await.unwrap(),
        Some(b"wf1".to_vec())
    );

    assert!(store.delete_bundle_state("hya/team", "k1").await.unwrap());
    assert!(!store.delete_bundle_state("hya/team", "k1").await.unwrap());
    assert_eq!(store.get_bundle_state("hya/team", "k1").await.unwrap(), None);
    assert_eq!(
        store.get_bundle_state("hya/workflow", "k1").await.unwrap(),
        Some(b"wf1".to_vec())
    );
}

#[tokio::test]
async fn bundle_state_json_roundtrip_and_rejects_empty_identifiers() {
    let store = SessionStore::connect_memory().await.unwrap();
    let cursor = json!({ "offset": 42, "tag": "ok" });
    store
        .set_bundle_state_json("hya/team", "cursor/main", &cursor)
        .await
        .unwrap();
    let decoded = store
        .get_bundle_state_json("hya/team", "cursor/main")
        .await
        .unwrap();
    assert_eq!(decoded, Some(cursor));

    let err = store
        .set_bundle_state("   ", "k", b"v")
        .await
        .expect_err("blank bundle must fail");
    assert!(matches!(err, StoreError::BundleStateData(_)));

    let err = store
        .set_bundle_state("hya/team", "", b"v")
        .await
        .expect_err("empty key must fail");
    assert!(matches!(err, StoreError::BundleStateData(_)));
}

#[tokio::test]
async fn bundle_state_compare_and_set_is_atomic() {
    let store = SessionStore::connect_memory().await.unwrap();

    // Create-if-absent succeeds once, fails on second race.
    assert!(
        store
            .compare_and_set_bundle_state("hya/team", "lock", None, Some(b"first"))
            .await
            .unwrap()
    );
    assert!(
        !store
            .compare_and_set_bundle_state("hya/team", "lock", None, Some(b"second"))
            .await
            .unwrap()
    );
    assert_eq!(
        store.get_bundle_state("hya/team", "lock").await.unwrap(),
        Some(b"first".to_vec())
    );

    // Wrong expected value fails; matching expected value succeeds.
    assert!(
        !store
            .compare_and_set_bundle_state("hya/team", "lock", Some(b"wrong"), Some(b"third"))
            .await
            .unwrap()
    );
    assert!(
        store
            .compare_and_set_bundle_state("hya/team", "lock", Some(b"first"), Some(b"third"))
            .await
            .unwrap()
    );
    assert_eq!(
        store.get_bundle_state("hya/team", "lock").await.unwrap(),
        Some(b"third".to_vec())
    );
    // Compare-and-delete when matching expected value.
    assert!(
        store
            .compare_and_set_bundle_state("hya/team", "lock", Some(b"third"), None)
            .await
            .unwrap()
    );
    assert_eq!(
        store.get_bundle_state("hya/team", "lock").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn bundle_state_prefix_listing_and_delete_treat_like_metacharacters_literally() {
    let store = SessionStore::connect_memory().await.unwrap();

    store
        .set_bundle_state("hya/demo", "ns%a/one", b"1")
        .await
        .unwrap();
    store
        .set_bundle_state("hya/demo", "ns%a/two", b"2")
        .await
        .unwrap();
    store
        .set_bundle_state("hya/demo", "nsXa/three", b"3")
        .await
        .unwrap();
    store
        .set_bundle_state("hya/demo", "other/four", b"4")
        .await
        .unwrap();

    let keys = store
        .list_bundle_state_keys("hya/demo", Some("ns%a/"))
        .await
        .unwrap();
    assert_eq!(keys, vec!["ns%a/one", "ns%a/two"]);

    let entries = store
        .list_bundle_state("hya/demo", Some("ns%a/"))
        .await
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].value, b"1");
    assert_eq!(entries[1].value, b"2");

    let all_keys = store.list_bundle_state_keys("hya/demo", None).await.unwrap();
    assert_eq!(
        all_keys,
        vec!["ns%a/one", "ns%a/two", "nsXa/three", "other/four"]
    );

    let deleted = store
        .delete_bundle_state_prefix("hya/demo", "ns%a/")
        .await
        .unwrap();
    assert_eq!(deleted, 2);
    assert_eq!(
        store.list_bundle_state_keys("hya/demo", None).await.unwrap(),
        vec!["nsXa/three", "other/four"]
    );
}

#[tokio::test]
async fn bundle_state_in_tx_commits_and_rolls_back_with_events() {
    let store = SessionStore::connect_memory().await.unwrap();
    let session = SessionId::new();

    // Rolled-back transaction writes neither event nor bundle state.
    {
        let mut tx = store.begin_transaction().await.unwrap();
        append_event_in_transaction(
            &mut tx,
            session,
            Event::Extension {
                session,
                bundle: "hya/workflow".into(),
                kind: "started".into(),
                payload: serde_json::json!({"id": 1}),
            },
        )
        .await
        .unwrap();
        SessionStore::set_bundle_state_in_tx(&mut tx, "hya/workflow", "run/1", b"pending")
            .await
            .unwrap();
        tx.rollback().await.unwrap();
    }
    assert!(store.replay(session).await.unwrap().is_empty());
    assert_eq!(
        store
            .get_bundle_state("hya/workflow", "run/1")
            .await
            .unwrap(),
        None
    );

    // Committed transaction atomically writes both event and bundle state.
    {
        let mut tx = store.begin_transaction().await.unwrap();
        append_event_in_transaction(
            &mut tx,
            session,
            Event::Extension {
                session,
                bundle: "hya/workflow".into(),
                kind: "started".into(),
                payload: serde_json::json!({"id": 2}),
            },
        )
        .await
        .unwrap();
        SessionStore::set_bundle_state_in_tx(&mut tx, "hya/workflow", "run/2", b"running")
            .await
            .unwrap();
        assert_eq!(
            SessionStore::get_bundle_state_in_tx(&mut tx, "hya/workflow", "run/2")
                .await
                .unwrap(),
            Some(b"running".to_vec())
        );
        tx.commit().await.unwrap();
    }
    assert_eq!(store.replay(session).await.unwrap().len(), 1);
    assert_eq!(
        store
            .get_bundle_state("hya/workflow", "run/2")
            .await
            .unwrap(),
        Some(b"running".to_vec())
    );
}
