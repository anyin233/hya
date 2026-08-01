#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Role controls selector mode only; ordinary roster is caller can_spawn.

mod support;

use std::sync::Arc;

use hya_bundle::AgentRole;
use hya_core::{EventBus, RuntimeRegistry, SessionEngine};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

#[tokio::test]
async fn can_spawn_roster_includes_reachable_subagent_excludes_unlisted_main_and_system() {
    // build (main) → research (subagent); plan (main) is catalog-present but not reachable.
    // Fixed system agents exist for exact lookup only.
    let catalog = support::test_catalog(&[
        ("build", AgentRole::Main, &["research"]),
        ("plan", AgentRole::Main, &[]),
        ("research", AgentRole::Subagent, &[]),
        ("compaction", AgentRole::Subagent, &[]),
        ("title", AgentRole::Subagent, &[]),
        ("summary", AgentRole::Subagent, &[]),
    ]);
    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(tools.snapshot(), catalog));
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, runtime, permission, EventBus::default());
    let workdir = support::TestDir::new("role-vs-roster");
    let binding = engine.bind_runtime(workdir.path()).unwrap();

    let roster = engine
        .agent_roster_for_binding(&binding, "build")
        .expect("caller roster");
    let names: Vec<&str> = roster.iter().map(|agent| agent.name.as_str()).collect();
    assert_eq!(names, vec!["research"], "ordinary roster: {names:?}");
    assert_eq!(
        roster[0].mode, "subagent",
        "role maps to mode on roster entries"
    );

    // Unlisted main and fixed system agents stay out of ordinary spawnability.
    assert!(binding.resolve_spawn("build", "plan").is_err());
    for reserved in ["compaction", "title", "summary"] {
        assert!(binding.resolve_spawn("build", reserved).is_err());
        assert!(
            binding.resolve_agent(reserved).is_some(),
            "{reserved} remains exact-lookup available"
        );
    }

    // Selector visibility is role/mode: main agents are primary, subagents are not.
    let build = binding.resolve_agent("build").expect("build");
    let research = binding.resolve_agent("research").expect("research");
    assert_eq!(build.role, AgentRole::Main);
    assert_eq!(research.role, AgentRole::Subagent);
}
