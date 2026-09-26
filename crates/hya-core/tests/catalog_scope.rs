//! Integration tests for `hya-core`: scoped catalog overlays in the runtime registry.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use hya_bundle::AgentRole;
use hya_core::{
    AgentModelConfiguration, CatalogScope, HookChain, HookDispatcher, RuntimeRefreshError,
    RuntimeRegistry, RuntimeSource, RuntimeSourceExport, RuntimeSourceId, ScopeKey, ScopeOverlay,
    TurnBinding,
};
use hya_proto::{ModelRef, ProjectId};
use hya_tool::{ToolPermission, ToolRegistry};
use support::{MarkerTool, TestDir, builtin_only_catalog, test_catalog};

fn registry() -> RuntimeRegistry {
    RuntimeRegistry::new(ToolRegistry::builtins(), builtin_only_catalog())
}

fn project(roots: &[&TestDir]) -> CatalogScope {
    CatalogScope::Project {
        id: ProjectId::new(),
        roots: roots.iter().map(|dir| dir.path().to_path_buf()).collect(),
    }
}

fn overlay(agent: &str) -> ScopeOverlay {
    ScopeOverlay::new(test_catalog(&[(agent, AgentRole::Main, &[])]))
}

fn tool_names(binding: &TurnBinding) -> BTreeSet<String> {
    binding
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect()
}

fn mcp_source(server: &str) -> RuntimeSource {
    let name = format!("mcp__{server}__lookup");
    RuntimeSource::new(
        RuntimeSourceId::mcp(server),
        [1; 32],
        Arc::new(()),
        vec![RuntimeSourceExport::tool(
            "lookup",
            name.clone(),
            Vec::new(),
            MarkerTool::new(name),
            ToolPermission::Mcp,
        )],
    )
}

fn plugin_source(id: &str, export: &str) -> RuntimeSource {
    RuntimeSource::new(
        RuntimeSourceId::plugin(id),
        [2; 32],
        Arc::new(()),
        vec![RuntimeSourceExport::tool(
            "tool",
            export,
            Vec::new(),
            MarkerTool::new(export),
            ToolPermission::Tool,
        )],
    )
}

fn hooks() -> Arc<dyn HookDispatcher> {
    Arc::new(HookChain::new(Vec::new()))
}

#[test]
fn two_project_scopes_resolve_their_own_agents_and_global_resolves_neither() {
    let (root_a, root_b) = (TestDir::new("scope-a"), TestDir::new("scope-b"));
    let registry = registry();
    let (scope_a, scope_b) = (project(&[&root_a]), project(&[&root_b]));
    registry
        .publish_scope(scope_a.key(), overlay("alpha-agent"))
        .unwrap();
    registry
        .publish_scope(scope_b.key(), overlay("beta-agent"))
        .unwrap();

    let a = registry.bind_scoped(&scope_a, root_a.path()).unwrap();
    let b = registry.bind_scoped(&scope_b, root_b.path()).unwrap();
    assert!(a.resolve_agent("alpha-agent").is_some());
    assert!(a.resolve_agent("beta-agent").is_none());
    assert!(b.resolve_agent("beta-agent").is_some());
    assert!(b.resolve_agent("alpha-agent").is_none());
    assert_eq!(a.scope(), &scope_a);
    assert_eq!(b.scope(), &scope_b);

    let global = registry
        .bind_scoped(&CatalogScope::Global, "".as_ref())
        .unwrap();
    assert!(global.resolve_agent("alpha-agent").is_none());
    assert!(global.resolve_agent("beta-agent").is_none());
    assert_eq!(global.scope(), &CatalogScope::Global);
    assert!(
        registry
            .bind_global()
            .unwrap()
            .resolve_agent("alpha-agent")
            .is_none()
    );

    // A Directory scope without an overlay binds the base, keyed by workdir.
    let directory = CatalogScope::Directory(root_a.path().to_path_buf());
    let plain = registry.bind_scoped(&directory, root_a.path()).unwrap();
    assert!(plain.resolve_agent("alpha-agent").is_none());
    assert_eq!(plain.workdir(), root_a.path());
    assert_eq!(
        plain.generation(),
        registry.bind_turn(root_a.path()).unwrap().generation()
    );
    assert!(plain.project_bundle_dirs().is_empty());
}

#[test]
fn a_base_mcp_publish_lazily_rebuilds_a_scope_and_keeps_its_overlay() {
    let root = TestDir::new("scope-mcp");
    let registry = registry();
    let scope = project(&[&root]);
    registry
        .publish_scope(scope.key(), overlay("alpha-agent"))
        .unwrap();
    let before = registry.bind_scoped(&scope, root.path()).unwrap();
    // Binding again with nothing changed reuses the cached scope snapshot.
    assert_eq!(
        registry
            .bind_scoped(&scope, root.path())
            .unwrap()
            .generation(),
        before.generation()
    );

    registry
        .refresh(|candidate| candidate.upsert_sources(vec![mcp_source("srv")]))
        .unwrap();
    let after = registry.bind_scoped(&scope, root.path()).unwrap();
    assert!(after.generation() > before.generation());
    assert!(tool_names(&after).contains("mcp__srv__lookup"));
    assert!(!tool_names(&before).contains("mcp__srv__lookup"));
    assert!(after.resolve_agent("alpha-agent").is_some());
}

#[test]
fn generations_are_unique_and_increasing_across_base_and_scopes() {
    let (root_a, root_b) = (TestDir::new("gen-a"), TestDir::new("gen-b"));
    let registry = registry();
    let (scope_a, scope_b) = (project(&[&root_a]), project(&[&root_b]));
    let base = registry.bind_turn(root_a.path()).unwrap().generation();
    let first_a = registry
        .publish_scope(scope_a.key(), overlay("alpha-agent"))
        .unwrap();
    let first_b = registry
        .publish_scope(scope_b.key(), overlay("beta-agent"))
        .unwrap();
    let refreshed = registry
        .refresh(|candidate| candidate.upsert_sources(vec![mcp_source("srv")]))
        .unwrap();
    let rebuilt_b = registry
        .bind_scoped(&scope_b, root_b.path())
        .unwrap()
        .generation();
    let rebuilt_a = registry
        .bind_scoped(&scope_a, root_a.path())
        .unwrap()
        .generation();
    let observed = [base, first_a, first_b, refreshed, rebuilt_b, rebuilt_a];
    assert!(
        observed.windows(2).all(|pair| pair[0] < pair[1]),
        "generations must be unique and increasing: {observed:?}"
    );
}

#[test]
fn a_failing_scope_compose_leaves_base_and_the_previous_scope_snapshot_unchanged() {
    let root = TestDir::new("scope-fail");
    let registry = registry();
    let scope = project(&[&root]);
    let base = registry.bind_turn(root.path()).unwrap();
    let good = registry
        .publish_scope(scope.key(), overlay("alpha-agent"))
        .unwrap();

    // Plugin sources must publish qualified names; a bare one is rejected.
    let mut bad = overlay("beta-agent");
    bad.plugin_sources = vec![plugin_source("proj", "bare_tool")];
    let failed = registry.publish_scope(scope.key(), bad);
    assert!(
        matches!(failed, Err(RuntimeRefreshError::ScopeCompose { .. })),
        "{failed:?}"
    );

    assert_eq!(
        registry.bind_turn(root.path()).unwrap().generation(),
        base.generation()
    );
    let kept = registry.bind_scoped(&scope, root.path()).unwrap();
    assert_eq!(kept.generation(), good);
    assert!(kept.resolve_agent("alpha-agent").is_some());
    assert!(kept.resolve_agent("beta-agent").is_none());
}

#[test]
fn a_failing_lazy_rebuild_is_typed_and_recovers_once_the_base_conflict_is_gone() {
    let root = TestDir::new("scope-lazy-fail");
    let registry = registry();
    let scope = project(&[&root]);
    let mut scoped = overlay("alpha-agent");
    scoped.plugin_sources = vec![plugin_source("proj", "proj__tool")];
    registry.publish_scope(scope.key(), scoped).unwrap();
    let before = registry.bind_scoped(&scope, root.path()).unwrap();

    // The base now publishes the same canonical name from another source.
    registry
        .refresh(|candidate| candidate.upsert_sources(vec![plugin_source("other", "proj__tool")]))
        .unwrap();
    let failed = registry.bind_scoped(&scope, root.path());
    assert!(
        matches!(failed, Err(RuntimeRefreshError::ScopeCompose { .. })),
        "{:?}",
        failed.err()
    );
    assert!(registry.scope_overlay(&scope.key()).is_some());
    assert!(before.resolve_tool("proj__tool").is_some());

    let removed = BTreeSet::from([RuntimeSourceId::plugin("other")]);
    registry
        .refresh(|candidate| {
            candidate.remove_sources(&removed);
            Ok(())
        })
        .unwrap();
    let recovered = registry.bind_scoped(&scope, root.path()).unwrap();
    assert!(recovered.generation() > before.generation());
    assert!(recovered.resolve_agent("alpha-agent").is_some());
}

#[test]
fn drop_scope_and_dropping_bindings_release_the_source_owner() {
    struct Owner;
    let root = TestDir::new("scope-drop");
    let registry = registry();
    let scope = project(&[&root]);
    let owner = Arc::new(Owner);
    let weak = Arc::downgrade(&owner);
    let mut scoped = overlay("alpha-agent");
    scoped.plugin_sources = vec![
        RuntimeSource::new(RuntimeSourceId::plugin("proj"), [3; 32], owner, Vec::new())
            .with_hooks(hooks()),
    ];
    registry.publish_scope(scope.key(), scoped).unwrap();
    let binding = registry.bind_scoped(&scope, root.path()).unwrap();

    registry.drop_scope(&scope.key());
    assert!(registry.scope_overlay(&scope.key()).is_none());
    assert!(weak.upgrade().is_some(), "a live binding keeps its sources");
    drop(binding);
    assert!(weak.upgrade().is_none(), "the owner is released");

    // After the drop the scope binds the base again.
    let rebound = registry.bind_scoped(&scope, root.path()).unwrap();
    assert!(rebound.resolve_agent("alpha-agent").is_none());
}

#[test]
fn a_scope_plugin_hooks_reach_only_bindings_of_that_scope() {
    let (root_a, root_b) = (TestDir::new("hooks-a"), TestDir::new("hooks-b"));
    let registry = registry();
    let (scope_a, scope_b) = (project(&[&root_a]), project(&[&root_b]));
    let mut overlay_a = overlay("alpha-agent");
    overlay_a.plugin_sources = vec![
        RuntimeSource::new(
            RuntimeSourceId::plugin("proj-a"),
            [4; 32],
            Arc::new(()),
            Vec::new(),
        )
        .with_hooks(hooks()),
    ];
    let mut overlay_b = overlay("beta-agent");
    overlay_b.plugin_sources = vec![
        RuntimeSource::new(
            RuntimeSourceId::plugin("proj-b"),
            [5; 32],
            Arc::new(()),
            Vec::new(),
        )
        .with_hooks(hooks()),
    ];
    registry.publish_scope(scope_a.key(), overlay_a).unwrap();
    registry.publish_scope(scope_b.key(), overlay_b).unwrap();

    // `with_hooks` wraps the dispatcher, so identity is checked against the
    // retained source dispatcher: stable per scope, distinct across scopes.
    let a = registry.bind_scoped(&scope_a, root_a.path()).unwrap();
    let chain_a = a.bundle_hooks_for_agent("build");
    assert_eq!(chain_a.len(), 1);
    let again = registry.bind_scoped(&scope_a, root_a.path()).unwrap();
    assert!(Arc::ptr_eq(
        &again.bundle_hooks_for_agent("build")[0],
        &chain_a[0]
    ));
    let alpha = a.bundle_hooks_for_agent("alpha-agent");
    assert!(alpha.iter().any(|hook| Arc::ptr_eq(hook, &chain_a[0])));

    let b = registry.bind_scoped(&scope_b, root_b.path()).unwrap();
    let chain_b = b.bundle_hooks_for_agent("build");
    assert_eq!(chain_b.len(), 1);
    assert!(!Arc::ptr_eq(&chain_b[0], &chain_a[0]));
    assert!(
        !b.bundle_hooks_for_agent("beta-agent")
            .iter()
            .any(|hook| Arc::ptr_eq(hook, &chain_a[0]))
    );

    assert!(
        registry
            .bind_turn(root_a.path())
            .unwrap()
            .bundle_hooks_for_agent("build")
            .is_empty()
    );
    registry.drop_scope(&scope_a.key());
    assert!(
        registry
            .bind_scoped(&scope_a, root_a.path())
            .unwrap()
            .bundle_hooks_for_agent("build")
            .is_empty()
    );
}

#[test]
fn scope_bundle_model_leaves_shadow_the_user_scope_configuration() {
    let root = TestDir::new("scope-models");
    let registry = registry();
    let scope = project(&[&root]);
    let bundle_id = "hya/test-alpha-agent".to_string();
    registry.publish_agent_model_configuration(AgentModelConfiguration {
        builtin: BTreeMap::from([("build".to_string(), ModelRef::new("base-build"))]),
        bundles: BTreeMap::from([(
            bundle_id.clone(),
            BTreeMap::from([("alpha-agent".to_string(), ModelRef::new("user-alpha"))]),
        )]),
    });
    let mut scoped = overlay("alpha-agent");
    scoped.project_bundle_dirs =
        BTreeMap::from([(bundle_id.clone(), root.path().join(".hya/bundles/alpha"))]);
    registry.publish_scope(scope.key(), scoped.clone()).unwrap();

    // A shadowing project bundle without its own leaves drops the user leaf.
    let binding = registry.bind_scoped(&scope, root.path()).unwrap();
    assert!(binding.configured_agent_model("alpha-agent").is_none());
    assert_eq!(
        binding.configured_agent_model("build"),
        Some(&ModelRef::new("base-build"))
    );
    assert_eq!(
        binding.project_bundle_dirs().get(&bundle_id),
        Some(&root.path().join(".hya/bundles/alpha"))
    );

    scoped.bundle_models = BTreeMap::from([(
        bundle_id.clone(),
        BTreeMap::from([("alpha-agent".to_string(), ModelRef::new("project-alpha"))]),
    )]);
    registry.publish_scope(scope.key(), scoped).unwrap();
    let binding = registry.bind_scoped(&scope, root.path()).unwrap();
    assert_eq!(
        binding.configured_agent_model("alpha-agent"),
        Some(&ModelRef::new("project-alpha"))
    );
    let fingerprint = registry
        .scope_overlay(&scope.key())
        .map(|overlay| overlay.fingerprint);
    assert_eq!(fingerprint, Some([0; 32]));
    assert_eq!(
        ScopeKey::Project(scope.project_id().unwrap()),
        scope.key(),
        "the key ignores roots"
    );
}
