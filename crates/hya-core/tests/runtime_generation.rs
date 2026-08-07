//! Integration tests for `hya-core`: runtime generation.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::collections::BTreeSet;

use hya_bundle::AgentRole;
use hya_core::RuntimeRegistry;
use hya_tool::{ToolPermission, ToolRegistry};

use support::{MarkerTool, TestDir, test_catalog};

fn tool_names(binding: &hya_core::TurnBinding) -> BTreeSet<String> {
    binding
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect()
}

fn skill_names(binding: &hya_core::TurnBinding) -> BTreeSet<String> {
    binding
        .skills()
        .iter()
        .map(|skill| skill.name.clone())
        .collect()
}

#[test]
fn tool_publication_keeps_the_bound_agent_catalog_pinned() {
    let workdir = TestDir::new("agent-catalog-binding");
    // `general` is a built-in, so pin an installed bundle agent instead.
    let catalog = test_catalog(&[("pinned-agent", AgentRole::Main, &[])]);
    let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);

    let before = registry.bind_turn(workdir.path()).unwrap();
    let before_agent = before.resolve_agent("pinned-agent").unwrap();
    assert_eq!(before_agent.prompt, Some("pinned-agent prompt"));

    registry
        .refresh(|candidate| candidate.register_tool(MarkerTool::new("after_publish")))
        .unwrap();
    let after = registry.bind_turn(workdir.path()).unwrap();

    assert!(std::ptr::eq(before.agent_catalog(), after.agent_catalog()));
    assert_eq!(
        before.resolve_agent("pinned-agent").unwrap(),
        after.resolve_agent("pinned-agent").unwrap()
    );
}

#[test]
fn catalog_publication_is_atomic_and_turn_bindings_pin_old_catalog() {
    let workdir = TestDir::new("catalog-publication");
    workdir.write_skill("stable_skill");
    let tools = ToolRegistry::builtins();
    tools
        .register_with_permission(MarkerTool::new("mcp__stable__marker"), ToolPermission::Mcp)
        .expect("register stable marker tool");
    let registry = RuntimeRegistry::new(tools, test_catalog(&[("general", AgentRole::Main, &[])]));

    let old_binding = registry.bind_turn(workdir.path()).unwrap();
    let old_generation = old_binding.generation();
    let candidate = test_catalog(&[
        ("general", AgentRole::Main, &[]),
        ("installed-agent", AgentRole::Main, &[]),
    ]);

    let published = registry.publish_catalog(candidate.clone()).unwrap();
    let fresh_binding = registry.bind_turn(workdir.path()).unwrap();

    assert_eq!(published.get(), old_generation.get() + 1);
    assert_eq!(fresh_binding.generation(), published);
    assert!(old_binding.resolve_agent("installed-agent").is_none());
    assert!(fresh_binding.resolve_agent("installed-agent").is_some());
    assert!(old_binding.resolve_agent("general").is_some());
    assert!(tool_names(&old_binding).contains("mcp__stable__marker"));
    assert!(skill_names(&old_binding).contains("stable_skill"));
    assert_eq!(tool_names(&old_binding), tool_names(&fresh_binding));
    assert_eq!(skill_names(&old_binding), skill_names(&fresh_binding));
    assert!(std::ptr::eq(
        fresh_binding.agent_catalog(),
        candidate.as_ref()
    ));
}

#[test]
fn requested_agent_and_roster_are_resolved_from_the_bound_spawn_graph() {
    let workdir = TestDir::new("agent-resolution");
    let catalog = test_catalog(&[
        ("general", AgentRole::Main, &[]),
        ("lead", AgentRole::Main, &["worker"]),
        ("worker", AgentRole::Main, &[]),
    ]);
    let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
    let binding = registry.bind_turn(workdir.path()).unwrap();

    assert_eq!(
        binding.resolve_requested_agent(None).unwrap().stable_id,
        "general"
    );
    assert!(matches!(
        binding.resolve_requested_agent(Some("missing")),
        Err(hya_bundle::BundleError::UnknownAgentId { .. })
    ));
    let roster = binding.spawnable_agents("lead").unwrap();
    assert_eq!(
        roster
            .iter()
            .map(|agent| agent.stable_id)
            .collect::<Vec<_>>(),
        ["worker"]
    );
    assert!(matches!(
        binding.resolve_spawn("lead", "compaction"),
        Err(hya_bundle::BundleError::AgentSpawnNotAllowed { .. })
    ));
}

#[test]
fn in_flight_turn_retains_generation_while_post_publish_turn_sees_next() {
    let workdir = TestDir::new("binding");
    let tools = ToolRegistry::builtins();
    tools
        .register_with_permission(MarkerTool::new("generation_n"), ToolPermission::Mcp)
        .unwrap();
    let registry = RuntimeRegistry::new(tools, test_catalog(&[("general", AgentRole::Main, &[])]));

    let in_flight = registry.bind_turn(workdir.path()).unwrap();
    let generation_n = in_flight.generation();

    let published = registry
        .refresh(|candidate| {
            candidate.remove_tool("generation_n");
            candidate.register_tool_with_permission(
                MarkerTool::new("generation_n_plus_1"),
                ToolPermission::Mcp,
            )
        })
        .unwrap();
    let post_publish = registry.bind_turn(workdir.path()).unwrap();

    assert_eq!(published.get(), generation_n.get() + 1);
    assert_eq!(in_flight.generation(), generation_n);
    assert_eq!(post_publish.generation(), published);
    assert!(tool_names(&in_flight).contains("generation_n"));
    assert!(!tool_names(&in_flight).contains("generation_n_plus_1"));
    assert!(!tool_names(&post_publish).contains("generation_n"));
    assert!(tool_names(&post_publish).contains("generation_n_plus_1"));
}

#[test]
fn one_turn_cannot_mix_tool_skill_or_mcp_members_across_generations() {
    let workdir = TestDir::new("complete-view");
    workdir.write_skill("skill_n");
    let tools = ToolRegistry::builtins();
    tools
        .register(MarkerTool::new("tool_n"))
        .expect("register generation N tool");
    tools
        .register_with_permission(MarkerTool::new("mcp__n__lookup"), ToolPermission::Mcp)
        .expect("register generation N MCP tool");
    let registry = RuntimeRegistry::new(tools, test_catalog(&[("general", AgentRole::Main, &[])]));
    let generation_n = registry.bind_turn(workdir.path()).unwrap();

    workdir.remove_skill("skill_n");
    workdir.write_skill("skill_n_plus_1");
    registry
        .refresh(|candidate| {
            candidate.remove_tool("tool_n");
            candidate.remove_tool("mcp__n__lookup");
            candidate.register_tool(MarkerTool::new("tool_n_plus_1"))?;
            candidate.register_tool_with_permission(
                MarkerTool::new("mcp__n_plus_1__lookup"),
                ToolPermission::Mcp,
            )?;
            candidate.refresh_skills(workdir.path());
            Ok(())
        })
        .unwrap();
    let generation_n_plus_1 = registry.bind_turn(workdir.path()).unwrap();

    let old_tools = tool_names(&generation_n);
    let old_skills = skill_names(&generation_n);
    assert!(old_tools.contains("tool_n"));
    assert!(old_tools.contains("mcp__n__lookup"));
    assert!(!old_tools.contains("tool_n_plus_1"));
    assert!(!old_tools.contains("mcp__n_plus_1__lookup"));
    assert!(old_skills.contains("skill_n"));
    assert!(!old_skills.contains("skill_n_plus_1"));

    let new_tools = tool_names(&generation_n_plus_1);
    let new_skills = skill_names(&generation_n_plus_1);
    assert!(!new_tools.contains("tool_n"));
    assert!(!new_tools.contains("mcp__n__lookup"));
    assert!(new_tools.contains("tool_n_plus_1"));
    assert!(new_tools.contains("mcp__n_plus_1__lookup"));
    assert!(!new_skills.contains("skill_n"));
    assert!(new_skills.contains("skill_n_plus_1"));
}
