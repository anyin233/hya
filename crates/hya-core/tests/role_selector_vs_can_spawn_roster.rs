#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Role controls selector mode only; the ordinary roster is the caller's scope.

mod support;

use std::sync::Arc;

use hya_bundle::AgentRole;
use hya_core::{EventBus, RuntimeRegistry, SessionEngine};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

async fn engine(catalog: Arc<hya_core::AgentCatalog>) -> SessionEngine {
    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(tools.snapshot(), catalog));
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    SessionEngine::new(store, providers, runtime, permission, EventBus::default())
}

#[tokio::test]
async fn a_builtin_caller_reaches_every_ordinary_agent_but_no_reserved_one() {
    // Built-ins spawn the whole ordinary set, so installing a bundle makes its
    // agent reachable with no edit to any built-in definition.
    let catalog = support::test_catalog(&[("research", AgentRole::Subagent, &[])]);
    let engine = engine(catalog).await;
    let workdir = support::TestDir::new("role-vs-roster-builtin");
    let binding = engine.bind_runtime(workdir.path()).unwrap();

    let roster = engine
        .agent_roster_for_binding(&binding, "build")
        .expect("caller roster");
    let names: Vec<&str> = roster.iter().map(|agent| agent.name.as_str()).collect();
    assert!(names.contains(&"research"), "ordinary roster: {names:?}");
    assert!(names.contains(&"plan"), "ordinary roster: {names:?}");
    assert_eq!(
        roster
            .iter()
            .find(|agent| agent.name == "research")
            .map(|agent| agent.mode.as_str()),
        Some("subagent"),
        "role maps to mode on roster entries"
    );

    for reserved in ["compaction", "title", "summary"] {
        assert!(!names.contains(&reserved));
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

#[tokio::test]
async fn a_bundle_caller_reaches_only_the_agents_it_lists() {
    let catalog = support::test_catalog(&[
        ("lead", AgentRole::Main, &["research"]),
        ("research", AgentRole::Subagent, &[]),
        ("unlisted", AgentRole::Main, &[]),
    ]);
    let engine = engine(catalog).await;
    let workdir = support::TestDir::new("role-vs-roster-bundle");
    let binding = engine.bind_runtime(workdir.path()).unwrap();

    let roster = engine
        .agent_roster_for_binding(&binding, "lead")
        .expect("caller roster");
    let names: Vec<&str> = roster.iter().map(|agent| agent.name.as_str()).collect();
    assert_eq!(names, vec!["research"], "ordinary roster: {names:?}");

    assert!(binding.resolve_spawn("lead", "research").is_ok());
    assert!(binding.resolve_spawn("lead", "unlisted").is_err());
    assert!(
        binding.resolve_spawn("lead", "general").is_err(),
        "a bundle agent does not inherit the builtin ordinary scope"
    );
}
