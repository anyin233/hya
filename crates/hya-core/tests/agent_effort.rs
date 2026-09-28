//! An Agent's own default thinking effort, as the main agent sees it through
//! `list_agents` (the roster annotation) and as requests resolve it.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use hya_core::{AgentEffortSource, EventBus, SessionEngine};
use hya_provider::{ProviderRouter, ReasoningEffort};
use hya_store::{OwnerRunId, SessionStore};
use hya_tool::{AgentDef, PermissionPlane, PermissionRules, ToolRegistry};

fn row(name: &str, authored: Option<&str>) -> AgentDef {
    AgentDef {
        name: name.to_string(),
        description: None,
        category: None,
        mode: "subagent".to_string(),
        effort: authored.map(str::to_string),
        effort_source: authored.map(|_| "authored".to_string()),
    }
}

#[tokio::test]
async fn roster_shows_runtime_over_configured_over_authored_agent_effort() {
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        store.clone(),
        Arc::new(ProviderRouter::new()),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    );
    engine
        .runtime_registry()
        .publish_agent_effort_configuration(
            [
                ("scout".to_string(), ReasoningEffort::Low),
                ("review".to_string(), ReasoningEffort::Medium),
            ]
            .into_iter()
            .collect(),
        );
    store
        .upsert_agent_effort_preference(owner, "review", "high", 1)
        .await
        .unwrap();

    let roster: Arc<[AgentDef]> = vec![
        row("scout", Some("max")),
        row("review", Some("low")),
        row("deep", Some("xhigh")),
        row("general", None),
    ]
    .into();
    let annotated = engine.annotate_agent_efforts(roster).await.unwrap();
    let shown: Vec<_> = annotated
        .iter()
        .map(|agent| {
            (
                agent.name.as_str(),
                agent.effort.as_deref(),
                agent.effort_source.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("scout", Some("low"), Some("configured")),
            ("review", Some("high"), Some("preference")),
            ("deep", Some("xhigh"), Some("authored")),
            ("general", None, None),
        ]
    );

    assert_eq!(
        engine.agent_effort("review", None).await.unwrap(),
        Some((ReasoningEffort::High, AgentEffortSource::Preference))
    );
    assert_eq!(engine.agent_effort("general", None).await.unwrap(), None);
}
