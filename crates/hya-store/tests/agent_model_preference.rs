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

fn temp_db() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "hya-agent-model-preference-{nanos}-{}-{id}.db",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn preferences_require_the_runtime_owner_and_remain_isolated() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    let build = preference("build", "openai", "gpt-5.6-sol");
    let general = preference("general", "anthropic", "claude-opus-4-1");

    let error = store
        .upsert_agent_model_preference(owner, &build)
        .await
        .expect_err("claim-less preference mutation must fail closed");
    assert!(matches!(error, StoreError::RuntimeOwnerClaimRequired));

    store
        .claim_runtime_owner(owner)
        .expect("claim runtime owner");
    store
        .upsert_agent_model_preference(owner, &general)
        .await
        .unwrap();
    store
        .upsert_agent_model_preference(owner, &build)
        .await
        .unwrap();

    assert_eq!(
        store.list_agent_model_preferences().await.unwrap(),
        vec![build.clone(), general]
    );

    let replacement = preference("build", "openai", "gpt-5.6-pro");
    store
        .upsert_agent_model_preference(owner, &replacement)
        .await
        .unwrap();
    assert_eq!(
        store.list_agent_model_preferences().await.unwrap(),
        vec![
            replacement,
            preference("general", "anthropic", "claude-opus-4-1")
        ]
    );

    store
        .remove_agent_model_preference(owner, &AgentName::new("build"))
        .await
        .unwrap();
    store
        .remove_agent_model_preference(owner, &AgentName::new("build"))
        .await
        .unwrap();
    assert_eq!(
        store.list_agent_model_preferences().await.unwrap(),
        vec![preference("general", "anthropic", "claude-opus-4-1")]
    );
}

#[tokio::test]
async fn preferences_survive_a_file_backed_store_reopen() {
    let path = temp_db();
    let build = preference("build", "anthropic", "claude-opus-4-1");
    let general = preference("general", "openai", "openai/gpt-5.6-sol");

    {
        let store = SessionStore::connect(&path).await.unwrap();
        let owner = OwnerRunId::new();
        store
            .claim_runtime_owner(owner)
            .expect("claim first runtime owner");
        store
            .upsert_agent_model_preference(owner, &general)
            .await
            .unwrap();
        store
            .upsert_agent_model_preference(owner, &build)
            .await
            .unwrap();
    }

    let reopened = SessionStore::connect(&path).await.unwrap();
    assert_eq!(
        reopened.list_agent_model_preferences().await.unwrap(),
        vec![build, general]
    );
}

/// Exact identity bounds fail before SQLite mutation, while boundary values remain valid.
#[tokio::test]
async fn preferences_validate_identity_bounds_before_mutation() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store
        .claim_runtime_owner(owner)
        .expect("claim runtime owner");

    let boundary = preference(&"a".repeat(1_024), &"p".repeat(1_024), &"m".repeat(4_096));
    store
        .upsert_agent_model_preference(owner, &boundary)
        .await
        .expect("boundary-sized preference must be accepted");

    let invalid = [
        preference("", "hya", "offline"),
        preference(&"a".repeat(1_025), "hya", "offline"),
        preference("build", "", "offline"),
        preference("build", &"p".repeat(1_025), "offline"),
        preference("build", "hya", ""),
        preference("build", "hya", &"m".repeat(4_097)),
    ];
    for candidate in invalid {
        let error = store
            .upsert_agent_model_preference(owner, &candidate)
            .await
            .expect_err("invalid preference identity must fail closed");
        assert!(matches!(error, StoreError::AdmissionData(_)));
    }

    assert_eq!(
        store.list_agent_model_preferences().await.unwrap(),
        vec![boundary]
    );
}

/// Concurrent valid clients preserve disjoint Agent rows under the serialized writer boundary.
#[tokio::test]
async fn concurrent_disjoint_preferences_are_not_lost() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store
        .claim_runtime_owner(owner)
        .expect("claim runtime owner");
    let build = preference("build", "hya", "offline");
    let general = preference("general", "openai", "gpt-5.6-sol");
    let build_store = store.clone();
    let general_store = store.clone();

    let (build_result, general_result) = tokio::join!(
        build_store.upsert_agent_model_preference(owner, &build),
        general_store.upsert_agent_model_preference(owner, &general),
    );
    build_result.expect("first disjoint preference must commit");
    general_result.expect("second disjoint preference must commit");

    assert_eq!(
        store.list_agent_model_preferences().await.unwrap(),
        vec![build, general]
    );
}
