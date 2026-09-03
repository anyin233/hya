//! App-owned durable Agent model preference control.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use hya_app::{AgentModelControlError, AgentModelIdentity, PersistentAgentModelControl};
use hya_proto::{AgentName, ModelRef, OwnerRunId};
use hya_provider::{DevProvider, ProviderRouter};
use hya_store::{AgentModelPreference, SessionStore};
use hya_tool::ToolRegistry;

#[tokio::test]
async fn control_loads_validates_persists_and_publishes_preferences() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store
        .claim_runtime_owner(owner)
        .expect("claim runtime owner");
    store
        .upsert_agent_model_preference(
            owner,
            &AgentModelPreference {
                agent: AgentName::new("general"),
                provider_id: "hya".to_string(),
                model_id: "offline".to_string(),
            },
        )
        .await
        .unwrap();

    let runtime = support::test_runtime(Arc::new(ToolRegistry::builtins()), &[]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new())));
    let control = PersistentAgentModelControl::load(store.clone(), owner, runtime.clone(), router)
        .await
        .unwrap();

    let loaded = runtime.bind_turn(std::path::Path::new(".")).unwrap();
    assert_eq!(
        loaded.agent_model_preference("general"),
        Some(&ModelRef::new("hya/offline"))
    );
    assert_eq!(
        control
            .effective_model(&loaded, "general", &ModelRef::new("fallback"))
            .unwrap(),
        ModelRef::new("hya/offline")
    );

    let binding = runtime.bind_turn(std::path::Path::new(".")).unwrap();
    control
        .set(
            &binding,
            "title",
            Some(AgentModelIdentity::new("hya", "offline")),
        )
        .await
        .unwrap();
    let after_set = runtime.bind_turn(std::path::Path::new(".")).unwrap();
    assert_eq!(
        after_set.agent_model_preference("title"),
        Some(&ModelRef::new("hya/offline"))
    );

    control.set(&binding, "general", None).await.unwrap();
    let after_clear = runtime.bind_turn(std::path::Path::new(".")).unwrap();
    assert_eq!(after_clear.agent_model_preference("general"), None);
    assert_eq!(
        after_clear.agent_model_preference("title"),
        Some(&ModelRef::new("hya/offline"))
    );

    let rows = store.list_agent_model_preferences().await.unwrap();
    assert_eq!(
        rows,
        vec![AgentModelPreference {
            agent: AgentName::new("title"),
            provider_id: "hya".to_string(),
            model_id: "offline".to_string(),
        }]
    );

    let unknown = control
        .set(
            &binding,
            "missing-agent",
            Some(AgentModelIdentity::new("hya", "offline")),
        )
        .await
        .expect_err("unknown Agent must fail closed");
    assert!(matches!(
        unknown,
        AgentModelControlError::UnknownAgent { .. }
    ));

    let unavailable = control
        .set(
            &binding,
            "general",
            Some(AgentModelIdentity::new("openai", "missing")),
        )
        .await
        .expect_err("model outside exact provider catalog must fail closed");
    assert!(matches!(
        unavailable,
        AgentModelControlError::ModelUnavailable { .. }
    ));
}

#[tokio::test]
async fn failed_owner_fenced_mutation_keeps_the_published_snapshot() {
    let store = SessionStore::connect_memory().await.unwrap();
    let claimed_owner = OwnerRunId::new();
    store
        .claim_runtime_owner(claimed_owner)
        .expect("claim runtime owner");
    let runtime = support::test_runtime(Arc::new(ToolRegistry::builtins()), &[]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new())));
    let control =
        PersistentAgentModelControl::load(store, OwnerRunId::new(), runtime.clone(), router)
            .await
            .unwrap();
    let binding = runtime.bind_turn(std::path::Path::new(".")).unwrap();

    let error = control
        .set(
            &binding,
            "general",
            Some(AgentModelIdentity::new("hya", "offline")),
        )
        .await
        .expect_err("stale owner must not mutate preferences");
    assert!(matches!(
        error,
        AgentModelControlError::Store(hya_store::StoreError::RuntimeOwnerClaimRequired)
    ));
    let after = runtime.bind_turn(std::path::Path::new(".")).unwrap();
    assert_eq!(after.agent_model_preference("general"), None);
}
