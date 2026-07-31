#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::collections::BTreeSet;

use hya_core::RuntimeRegistry;
use hya_tool::{ToolPermission, ToolRegistry};

use support::{MarkerTool, TestDir};

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
fn in_flight_turn_retains_generation_while_post_publish_turn_sees_next() {
    let workdir = TestDir::new("binding");
    let tools = ToolRegistry::builtins();
    tools
        .register_with_permission(MarkerTool::new("generation_n"), ToolPermission::Mcp)
        .unwrap();
    let registry = RuntimeRegistry::new(tools);

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
    let registry = RuntimeRegistry::new(tools);
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
