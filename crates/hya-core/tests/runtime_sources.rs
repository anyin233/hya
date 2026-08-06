//! Integration tests for `hya-core`: runtime sources.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use hya_bundle::AgentRole;
use hya_core::{
    RuntimeRefreshError, RuntimeRegistry, RuntimeSource, RuntimeSourceExport, RuntimeSourceId,
};
use hya_tool::{ToolPermission, ToolRegistry};

use support::{MarkerTool, TestDir, test_catalog};

fn source(
    id: RuntimeSourceId,
    digest: u8,
    declared: &str,
    canonical: &str,
    aliases: Vec<String>,
) -> RuntimeSource {
    RuntimeSource::new(
        id,
        [digest; 32],
        Arc::new(()),
        vec![RuntimeSourceExport::tool(
            declared,
            canonical,
            aliases,
            MarkerTool::new(canonical),
            ToolPermission::Mcp,
        )],
    )
}

fn assert_rejected_without_generation(
    registry: &RuntimeRegistry,
    workdir: &TestDir,
    build: impl FnOnce(&mut hya_core::RuntimeCandidate) -> Result<(), RuntimeRefreshError>,
    expected: &str,
) {
    let before = registry.bind_turn(workdir.path()).unwrap();
    let error = registry
        .refresh(build)
        .expect_err("candidate must fail closed");
    assert!(
        error.to_string().contains(expected),
        "expected {expected:?} in {error}"
    );
    let after = registry.bind_turn(workdir.path()).unwrap();
    assert_eq!(after.generation(), before.generation());
}

#[test]
fn duplicate_source_and_canonical_alias_collisions_reject_before_generation() {
    let workdir = TestDir::new("runtime-source-collisions");
    let registry = RuntimeRegistry::new(
        ToolRegistry::builtins(),
        test_catalog(&[("general", AgentRole::Main, &[])]),
    );
    let duplicate_id = RuntimeSourceId::mcp("duplicate");
    assert_rejected_without_generation(
        &registry,
        &workdir,
        |candidate| {
            candidate.upsert_sources(vec![
                source(duplicate_id.clone(), 1, "one", "source_one", Vec::new()),
                source(duplicate_id, 2, "two", "source_two", Vec::new()),
            ])
        },
        "duplicate runtime source mcp:duplicate",
    );

    assert_rejected_without_generation(
        &registry,
        &workdir,
        |candidate| {
            candidate.upsert_sources(vec![source(
                RuntimeSourceId::plugin("canonical"),
                3,
                "read",
                "read",
                Vec::new(),
            )])
        },
        "duplicate tool name: read",
    );

    assert_rejected_without_generation(
        &registry,
        &workdir,
        |candidate| {
            candidate.upsert_sources(vec![source(
                RuntimeSourceId::plugin("alias"),
                4,
                "unique",
                "unique_source_tool",
                vec!["read".to_string()],
            )])
        },
        "duplicate tool name: read",
    );
}
