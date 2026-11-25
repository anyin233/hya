//! Durable per-Agent model preference behavior.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::{AgentName, OwnerRunId};
use hya_store::{AgentModelPreference, SessionStore, StoreError};

fn preference(agent: &str, provider_id: &str, model_id: &str) -> AgentModelPreference {
    AgentModelPreference {
        agent: AgentName::new(agent),
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
    }
}

struct TempDb(String);

impl TempDb {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        Self(
            std::env::temp_dir()
                .join(format!(
                    "hya-agent-model-preference-{nanos}-{}-{id}.db",
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
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0));
        }
    }
}

#[tokio::test]
async fn preferences_require_owner_persist_across_reopen_and_handle_concurrent_writes() {
    let db = TempDb::new();
    let owner = OwnerRunId::new();
    let build = preference("build", "openai", "gpt-5.6-sol");
    let general = preference("general", "anthropic", "claude-opus-4-1");
    let replacement = preference("build", "openai", "gpt-5.6-pro");

    {
        let store = SessionStore::connect(&db.0).await.unwrap();
        assert!(matches!(
            store.upsert_agent_model_preference(owner, &build).await,
            Err(StoreError::RuntimeOwnerClaimRequired)
        ));
        store.claim_runtime_owner(owner).unwrap();
        let (r1, r2) = tokio::join!(
            store.upsert_agent_model_preference(owner, &build),
            store.upsert_agent_model_preference(owner, &general),
        );
        r1.unwrap();
        r2.unwrap();
        assert_eq!(
            store.list_agent_model_preferences().await.unwrap(),
            vec![build.clone(), general.clone()]
        );

        store
            .upsert_agent_model_preference(owner, &replacement)
            .await
            .unwrap();
        assert_eq!(
            store.list_agent_model_preferences().await.unwrap(),
            vec![replacement.clone(), general.clone()]
        );
    }

    let reopened = SessionStore::connect(&db.0).await.unwrap();
    assert_eq!(
        reopened.list_agent_model_preferences().await.unwrap(),
        vec![replacement, general.clone()]
    );
    let next_owner = OwnerRunId::new();
    reopened.claim_runtime_owner(next_owner).unwrap();
    reopened
        .remove_agent_model_preference(next_owner, &AgentName::new("build"))
        .await
        .unwrap();
    reopened
        .remove_agent_model_preference(next_owner, &AgentName::new("build"))
        .await
        .unwrap();
    assert_eq!(
        reopened.list_agent_model_preferences().await.unwrap(),
        vec![general]
    );
}

#[tokio::test]
async fn preferences_validate_bounds_blank_identities_and_legacy_rows() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();

    // Boundary-sized and padded non-blank values are accepted verbatim.
    let boundary = preference(&"a".repeat(1_024), &"p".repeat(1_024), &"m".repeat(4_096));
    let exact = preference(" build ", " hya ", " offline ");
    store
        .upsert_agent_model_preference(owner, &boundary)
        .await
        .unwrap();
    store
        .upsert_agent_model_preference(owner, &exact)
        .await
        .unwrap();

    // Out-of-bounds and blank identities fail closed before mutation.
    for (candidate, expected_field) in [
        (preference("", "hya", "offline"), "agent_id"),
        (preference(&"a".repeat(1_025), "hya", "offline"), "agent_id"),
        (preference(" \t", "hya", "offline"), "agent_id"),
        (preference("build", "", "offline"), "provider_id"),
        (
            preference("build", &"p".repeat(1_025), "offline"),
            "provider_id",
        ),
        (preference("build", " \t", "offline"), "provider_id"),
        (preference("build", "hya", ""), "model_id"),
        (preference("build", "hya", &"m".repeat(4_097)), "model_id"),
        (preference("build", "hya", " \t"), "model_id"),
    ] {
        let error = store
            .upsert_agent_model_preference(owner, &candidate)
            .await
            .unwrap_err();
        assert!(
            matches!(error, StoreError::InvalidPreferenceData { field, .. } if field == expected_field),
            "expected {expected_field}, got {error:?}"
        );
    }
    assert!(matches!(
        store
            .remove_agent_model_preference(owner, &AgentName::new(" \t"))
            .await,
        Err(StoreError::InvalidPreferenceData {
            field: "agent_id",
            ..
        })
    ));
    assert_eq!(
        store.list_agent_model_preferences().await.unwrap(),
        vec![exact, boundary]
    );

    // Legacy rows with blank identities fail closed on read.
    let mut tx = store.begin_transaction().await.unwrap();
    sqlx::query(
        "INSERT INTO agent_model_preference (agent_id, provider_id, model_id) VALUES (?, ?, ?)",
    )
    .bind("legacy")
    .bind(" \t")
    .bind("offline")
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        store.list_agent_model_preferences().await,
        Err(StoreError::InvalidPreferenceData {
            field: "provider_id",
            ..
        })
    ));
}
