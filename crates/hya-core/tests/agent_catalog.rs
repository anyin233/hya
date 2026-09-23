#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `AgentCatalog` joins compiled-in built-ins with installed AgentBundles.
//!
//! Call sites resolve through one seam and read the origin off the result;
//! they never branch on "is this a bundle".

use std::sync::Arc;

use hya_bundle::{
    AgentRole, BundleCatalog, BundleError, BundleIdentity, ModelPolicy, PreparedAgent,
    PreparedAgentBundle, PreparedInstallableBundle, ResourceView,
};
use hya_core::{AgentCatalog, AgentOrigin, builtin_agents, core_agents_preset};
use hya_proto::AgentName;

/// One installed bundle holding one agent with the given spawn graph.
fn installed(bundle_id: &str, agent_id: &str, can_spawn: &[&str]) -> PreparedInstallableBundle {
    PreparedInstallableBundle::Agent(Box::new(PreparedAgentBundle {
        format_version: 2,
        identity: BundleIdentity {
            id: bundle_id.to_string(),
            version: "1.0.0".to_string(),
            publisher: "tests".to_string(),
        },
        namespace: None,
        digest: format!("digest-{bundle_id}"),
        agent: PreparedAgent {
            id: AgentName::new(agent_id),
            description: Some(format!("{agent_id} description")),
            role: AgentRole::Subagent,
            color: None,
            prompt: Some(format!("{agent_id} prompt")),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            legacy_spawn_lifecycle: None,
            resource_view: ResourceView::default(),
            can_spawn: can_spawn.iter().map(|id| AgentName::new(*id)).collect(),
            hook_refs: Vec::new(),
        },
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
    }))
}

fn catalog(bundles: &[PreparedInstallableBundle]) -> AgentCatalog {
    let bundles = BundleCatalog::from_prepared(bundles).expect("bundle catalog");
    AgentCatalog::new(Arc::new(bundles)).expect("agent catalog")
}

fn empty_catalog() -> AgentCatalog {
    catalog(&[])
}

#[test]
fn core_agents_are_backed_by_the_verified_embedded_preset() {
    let preset = core_agents_preset().expect("embedded core-agents preset");
    assert_eq!(preset.bundle_id(), "hya/core-agents");
    assert!(!preset.prepared_bytes().is_empty());
    assert_eq!(preset.digest().len(), 64);

    let catalog = empty_catalog();
    let preset_ids = preset
        .agents()
        .iter()
        .map(|agent| agent.id.as_str())
        .collect::<Vec<_>>();
    let catalog_ids = catalog
        .all()
        .iter()
        .map(|agent| agent.stable_id)
        .collect::<Vec<_>>();
    assert_eq!(catalog_ids, preset_ids);

    for (compat, prepared) in builtin_agents().iter().zip(preset.agents()) {
        assert_eq!(compat.id, prepared.id.as_str());
        assert_eq!(compat.description, prepared.description.as_deref());
        assert_eq!(compat.role, prepared.role);
        assert_eq!(compat.prompt, prepared.prompt.as_deref());
        assert_eq!(compat.model_policy.to_model_policy(), prepared.model_policy);
        assert_eq!(compat.system_reserved, preset.is_reserved(compat.id));
    }

    for definition in catalog.all() {
        assert!(definition.origin.is_builtin());
        assert!(definition.origin.is_preset());
        assert_eq!(
            definition.origin.preset_bundle_id(),
            Some("hya/core-agents")
        );
        assert!(definition.model_policy.model.is_none());
        assert!(definition.model_policy.category.is_none());
        assert!(definition.model_policy.reasoning.is_none());
    }
}

#[test]
fn core_agents_resolve_through_their_preset_qualified_identity() {
    let catalog = empty_catalog();
    let bare = catalog.resolve("build").expect("bare preset agent");
    let qualified = catalog
        .resolve("bundle:hya/core-agents/agent/build")
        .expect("qualified preset agent");
    assert_eq!(bare, qualified);
    assert_eq!(qualified.origin.preset_bundle_id(), Some("hya/core-agents"));
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

#[test]
fn agent_set_members_use_bundle_origin_and_existing_spawn_authorization() {
    let prepared = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "set-runtime",
        vec![hya_bundle::SourceFile::new(
            "bundle.yaml",
            br#"kind: AgentSetBundle
identity: { id: acme/team, version: 1.0.0, publisher: acme }
agents:
  - { id: team-lead, role: main, can_spawn: [team-worker] }
  - { id: team-worker, role: subagent }
"#,
        )],
    ))
    .expect("prepare");
    let bundles = BundleCatalog::from_verified_catalogs(&[&prepared]).expect("verified catalog");
    let catalog = AgentCatalog::new(Arc::new(bundles)).expect("runtime catalog");
    let worker = catalog
        .resolve_spawn("team-lead", "team-worker")
        .expect("allowed spawn");
    assert_eq!(
        worker.origin,
        AgentOrigin::Bundle {
            bundle_id: "acme/team"
        }
    );
    assert!(matches!(
        catalog.resolve_spawn("team-worker", "team-lead"),
        Err(BundleError::AgentSpawnNotAllowed { .. })
    ));
    assert!(
        catalog
            .resolve("bundle:acme/team/agent/team-worker")
            .is_some()
    );
}

#[test]
fn first_party_subagent_bundle_exposes_one_resident_worker() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../bundles/first-party/subagents");
    let prepared = hya_bundle::prepare_package(
        hya_bundle::BundleSource::read_directory(root).expect("subagent bundle source"),
    )
    .expect("prepare subagent bundle");
    let bundles = BundleCatalog::from_verified_catalogs(&[&prepared]).expect("verified catalog");
    let catalog = AgentCatalog::new(Arc::new(bundles)).expect("runtime catalog");

    let worker = catalog
        .resolve_spawn("build", "hya-worker")
        .expect("resident worker");
    assert_eq!(
        worker.origin,
        AgentOrigin::Bundle {
            bundle_id: "hya/subagents"
        }
    );
    assert!(!worker.origin.is_preset());
    assert!(!catalog.is_reserved(worker.stable_id));
    // The transient/resident split is gone with `spawn_lifecycle`.
    for removed in ["hya-transient-worker", "hya-resident-worker"] {
        assert!(
            catalog.resolve_spawn("build", removed).is_err(),
            "`{removed}` must no longer exist"
        );
    }
}

#[test]
fn core_agents_preset_is_the_runtime_loaded_first_party_bundle() {
    let preset = core_agents_preset().expect("core-agents preset");
    let catalog = hya_bundle::first_party_bundle("hya/core-agents").expect("load core-agents");
    assert!(std::ptr::eq(preset.prepared_bytes(), catalog.bytes()));
    assert_eq!(preset.digest(), catalog.digest());
    let roster = hya_core::builtin_agents::builtin_agents();
    assert_eq!(roster.len(), preset.agents().len());
    for (agent, prepared) in roster.iter().zip(preset.agents()) {
        assert!(std::ptr::eq(agent.id, prepared.id.as_str()));
    }
}
