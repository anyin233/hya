//! `AgentCatalog` joins compiled-in built-ins with installed AgentBundles.
//!
//! Call sites resolve through one seam and read the origin off the result;
//! they never branch on "is this a bundle".

use std::sync::Arc;

use hya_bundle::{
    AgentRole, BundleCatalog, BundleError, BundleIdentity, BundleOrigin, HarnessAccess,
    ModelPolicy, PreparedAgent, PreparedBundle, ResourceView, SpawnLifecycle,
};
use hya_core::{AgentCatalog, AgentOrigin};
use hya_proto::AgentName;

/// One installed bundle holding one agent with the given spawn graph.
fn installed(bundle_id: &str, agent_id: &str, can_spawn: &[&str]) -> PreparedBundle {
    PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: bundle_id.to_string(),
            version: "1.0.0".to_string(),
            publisher: "tests".to_string(),
        },
        origin: BundleOrigin::Installed,
        immutable: false,
        digest: format!("digest-{bundle_id}"),
        agents: vec![PreparedAgent {
            local_id: agent_id.to_string(),
            stable_id: AgentName::new(agent_id),
            description: Some(format!("{agent_id} description")),
            role: AgentRole::Subagent,
            color: None,
            prompt: Some(format!("{agent_id} prompt")),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            spawn_lifecycle: SpawnLifecycle::Transient,
            harness_access: HarnessAccess::Full,
            resource_view: ResourceView::default(),
            can_spawn: can_spawn.iter().map(|id| AgentName::new(*id)).collect(),
            hook_refs: Vec::new(),
        }],
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
    }
}

fn catalog(bundles: &[PreparedBundle]) -> AgentCatalog {
    let bundles = BundleCatalog::from_prepared(bundles).expect("bundle catalog");
    AgentCatalog::new(Arc::new(bundles)).expect("agent catalog")
}

fn empty_catalog() -> AgentCatalog {
    catalog(&[])
}

#[test]
fn resolves_builtins_with_zero_installed_bundles() {
    let catalog = empty_catalog();
    for id in ["build", "plan", "explore", "general", "hya-main", "title"] {
        let definition = catalog.resolve(id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(definition.stable_id, id);
        assert_eq!(definition.origin, AgentOrigin::Builtin);
    }
}

#[test]
fn resolves_a_bundle_agent_and_reports_its_bundle_origin() {
    let catalog = catalog(&[installed("acme/reviewer", "acme-reviewer", &[])]);
    let definition = catalog.resolve("acme-reviewer").expect("bundle agent");
    assert_eq!(
        definition.origin,
        AgentOrigin::Bundle {
            bundle_id: "acme/reviewer"
        }
    );
    assert_eq!(definition.origin.bundle_id(), Some("acme/reviewer"));
    assert!(!definition.origin.is_builtin());
}

#[test]
fn resolves_a_bundle_agent_by_qualified_reference() {
    let catalog = catalog(&[installed("acme/reviewer", "acme-reviewer", &[])]);
    let definition = catalog
        .resolve("bundle:acme/reviewer/agent/acme-reviewer")
        .expect("qualified reference");
    assert_eq!(definition.stable_id, "acme-reviewer");
}

#[test]
fn an_installed_bundle_may_not_shadow_a_builtin_agent_id() {
    let bundles = BundleCatalog::from_prepared(&[installed("acme/impostor", "build", &[])])
        .expect("bundle catalog");
    let error = AgentCatalog::new(Arc::new(bundles)).expect_err("shadowing must be rejected");
    assert_eq!(
        error,
        BundleError::BuiltinAgentIdShadowed {
            bundle_id: "acme/impostor".to_string(),
            agent_id: "build".to_string(),
        }
    );
}

#[test]
fn installing_a_bundle_makes_its_agent_spawnable_by_a_builtin() {
    let before = empty_catalog();
    assert!(
        !before
            .spawnable("build")
            .expect("roster")
            .iter()
            .any(|agent| agent.stable_id == "acme-reviewer")
    );

    let after = catalog(&[installed("acme/reviewer", "acme-reviewer", &[])]);
    let roster = after.spawnable("build").expect("roster");
    assert!(
        roster
            .iter()
            .any(|agent| agent.stable_id == "acme-reviewer"),
        "installing a bundle must not require editing any builtin definition"
    );
    assert!(after.resolve_spawn("build", "acme-reviewer").is_ok());
}

#[test]
fn reserved_system_agents_stay_unspawnable_by_ordinary_agents() {
    let catalog = catalog(&[installed("acme/reviewer", "acme-reviewer", &[])]);
    for reserved in ["compaction", "summary", "title"] {
        assert!(
            !catalog
                .spawnable("build")
                .expect("roster")
                .iter()
                .any(|agent| agent.stable_id == reserved),
            "`{reserved}` must not appear in an ordinary roster"
        );
        assert!(
            matches!(
                catalog.resolve_spawn("build", reserved),
                Err(BundleError::AgentSpawnNotAllowed { .. })
            ),
            "`{reserved}` must not be spawnable"
        );
        assert!(
            catalog.spawnable(reserved).expect("roster").is_empty(),
            "`{reserved}` must spawn nothing"
        );
    }
}

#[test]
fn a_bundle_agent_spawns_only_what_it_lists() {
    let catalog = catalog(&[
        installed("acme/lead", "acme-lead", &["explore", "acme-helper"]),
        installed("acme/helper", "acme-helper", &[]),
    ]);
    let roster = catalog.spawnable("acme-lead").expect("roster");
    let ids = roster
        .iter()
        .map(|agent| agent.stable_id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["explore", "acme-helper"]);

    assert!(catalog.resolve_spawn("acme-lead", "explore").is_ok());
    assert!(catalog.resolve_spawn("acme-lead", "acme-helper").is_ok());
    assert!(matches!(
        catalog.resolve_spawn("acme-lead", "general"),
        Err(BundleError::AgentSpawnNotAllowed { .. })
    ));
}

#[test]
fn a_missing_can_spawn_target_is_skipped_in_the_roster_but_errors_on_spawn() {
    // Bundles install independently. A dangling target must not brick the caller.
    let catalog = catalog(&[installed(
        "acme/lead",
        "acme-lead",
        &["explore", "not-installed"],
    )]);
    let ids = catalog
        .spawnable("acme-lead")
        .expect("roster")
        .iter()
        .map(|agent| agent.stable_id)
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec!["explore"],
        "an uninstalled target is skipped, not fatal"
    );
    assert!(matches!(
        catalog.resolve_spawn("acme-lead", "not-installed"),
        Err(BundleError::UnknownAgentId { .. })
    ));
}

#[test]
fn an_unknown_caller_is_an_error() {
    let catalog = empty_catalog();
    assert!(matches!(
        catalog.spawnable("no-such-agent"),
        Err(BundleError::UnknownAgentId { .. })
    ));
}

#[test]
fn the_ordinary_roster_holds_every_builtin_and_bundle_agent_sorted() {
    let catalog = catalog(&[
        installed("acme/zeta", "acme-zeta", &[]),
        installed("acme/alpha", "acme-alpha", &[]),
    ]);
    let ids = catalog
        .spawnable("build")
        .expect("roster")
        .iter()
        .map(|agent| agent.stable_id)
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            "acme-alpha",
            "acme-zeta",
            "build",
            "explore",
            "general",
            "hya-docs",
            "hya-explorer",
            "hya-implementer",
            "hya-main",
            "hya-planner",
            "hya-release",
            "hya-reviewer",
            "hya-tester",
            "plan",
        ]
    );
}

#[test]
fn the_builtin_digest_is_stable_across_catalogs() {
    let empty = empty_catalog();
    let populated = catalog(&[installed("acme/reviewer", "acme-reviewer", &[])]);
    assert_eq!(
        empty.builtin_digest(),
        populated.builtin_digest(),
        "installed bundles must not perturb the builtin roster digest"
    );
}
