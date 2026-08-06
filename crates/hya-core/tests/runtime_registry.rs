//! Integration tests for `hya-core`: runtime registry.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};

use hya_core::{RuntimeRefreshError, RuntimeRegistry};
use hya_tool::{ToolPermission, ToolRegistry};

use hya_bundle::AgentRole;
use support::{MarkerTool, TestDir, test_catalog};

fn tool_names(binding: &hya_core::TurnBinding) -> BTreeSet<String> {
    binding
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect()
}

#[test]
fn failed_candidate_refresh_retains_generation_and_exact_registry_view() {
    let workdir = TestDir::new("failed-refresh");
    let tools = ToolRegistry::builtins();
    tools
        .register(MarkerTool::new("stable"))
        .expect("register stable marker");
    let registry = RuntimeRegistry::new(tools, test_catalog(&[("general", AgentRole::Main, &[])]));
    let before = registry.bind_turn(workdir.path()).unwrap();
    let before_names = tool_names(&before);

    let failed = registry.refresh(|candidate| {
        candidate.remove_tool("stable");
        candidate.register_tool(MarkerTool::new("read"))
    });
    assert!(matches!(failed, Err(RuntimeRefreshError::DuplicateTool(_))));

    let after_failure = registry.bind_turn(workdir.path()).unwrap();
    assert_eq!(after_failure.generation(), before.generation());
    assert_eq!(tool_names(&after_failure), before_names);

    let published = registry
        .refresh(|candidate| candidate.register_tool(MarkerTool::new("after_failure")))
        .unwrap();
    assert_eq!(published.get(), before.generation().get() + 1);
}

#[test]
fn logically_unchanged_candidate_does_not_advance_generation() {
    let workdir = TestDir::new("no-op-refresh");
    workdir.write_skill("stable_skill");
    let tools = ToolRegistry::builtins();
    let stable = MarkerTool::new("stable_tool");
    tools
        .register(stable.clone())
        .expect("register stable marker");
    let registry = RuntimeRegistry::new(tools, test_catalog(&[("general", AgentRole::Main, &[])]));
    let before = registry.bind_turn(workdir.path()).unwrap();
    let before_names = tool_names(&before);

    let unchanged = registry
        .refresh(|candidate| {
            candidate.remove_tool("stable_tool");
            candidate.register_tool(stable)?;
            candidate.refresh_skills(workdir.path());
            Ok(())
        })
        .unwrap();
    let after = registry.bind_turn(workdir.path()).unwrap();

    assert_eq!(unchanged, before.generation());
    assert_eq!(after.generation(), before.generation());
    assert_eq!(tool_names(&after), before_names);
}

#[test]
fn concurrent_publications_are_unique_monotonic_and_never_publish_a_mixed_candidate() {
    const REFRESHES: usize = 8;

    let workdir = Arc::new(TestDir::new("concurrent-refresh"));
    let registry = Arc::new(RuntimeRegistry::new(
        ToolRegistry::builtins(),
        test_catalog(&[("general", AgentRole::Main, &[])]),
    ));
    let first = registry.bind_turn(workdir.path()).unwrap().generation();
    let barrier = Arc::new(Barrier::new(REFRESHES));

    let threads = (0..REFRESHES)
        .map(|index| {
            let registry = registry.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let generation = registry
                    .refresh(|candidate| {
                        for previous in 0..REFRESHES {
                            candidate.remove_tool(&format!("candidate_{previous}_tool"));
                            candidate.remove_tool(&format!("mcp__candidate_{previous}__lookup"));
                        }
                        candidate
                            .register_tool(MarkerTool::new(format!("candidate_{index}_tool")))?;
                        candidate.register_tool_with_permission(
                            MarkerTool::new(format!("mcp__candidate_{index}__lookup")),
                            ToolPermission::Mcp,
                        )
                    })
                    .unwrap();
                (generation, index)
            })
        })
        .collect::<Vec<_>>();

    let mut publications = threads
        .into_iter()
        .map(|thread| thread.join().expect("refresh thread"))
        .collect::<Vec<_>>();
    publications.sort_by_key(|(generation, _)| generation.get());

    let observed = publications
        .iter()
        .map(|(generation, _)| generation.get())
        .collect::<Vec<_>>();
    let expected = (1..=REFRESHES)
        .map(|offset| first.get() + offset as u64)
        .collect::<Vec<_>>();
    assert_eq!(observed, expected);

    let (_, final_candidate) = publications.last().expect("at least one publication");
    let active = tool_names(&registry.bind_turn(workdir.path()).unwrap());
    for index in 0..REFRESHES {
        assert_eq!(
            active.contains(&format!("candidate_{index}_tool")),
            index == *final_candidate
        );
        assert_eq!(
            active.contains(&format!("mcp__candidate_{index}__lookup")),
            index == *final_candidate
        );
    }
}
