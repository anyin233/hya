//! Trusted first-party presets are visible without becoming installable bundles.

#![allow(clippy::expect_used)]

use sha2::{Digest, Sha256};

#[test]
fn builtin_skill_catalog_comes_from_prepared_core_skills_bundle() {
    let bytes = hya_engine::skill_catalog::core_skills_preset_bytes();
    let digest = format!("{:x}", Sha256::digest(bytes));
    let catalog = hya_bundle::PreparedCatalog::decode(bytes, &digest).expect("core Skill catalog");
    let bundle = &catalog.bundles()[0];
    let builtins = hya_engine::skill_catalog::builtin_skills();
    assert_eq!(bundle.identity().id, "hya/core-skills");
    assert_eq!(bundle.skills().len(), builtins.len());
    for (resource, builtin) in bundle.skills().iter().zip(builtins) {
        let parsed =
            hya_engine::skill_catalog::parse_skill(&resource.content).expect("valid bundled Skill");
        assert_eq!(resource.local_id, builtin.name);
        assert_eq!(parsed.name, builtin.name);
        assert_eq!(parsed.description, builtin.description);
        assert_eq!(parsed.content, builtin.content);
        assert!(builtin.path.to_string_lossy().contains("hya/core-skills"));
    }
}

#[test]
fn inventory_exposes_immutable_noninstallable_presets_and_internal_bundles() {
    let inventory = hya_app::trusted_preset_inventory().expect("trusted preset inventory");
    assert_eq!(
        inventory
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        [
            "hya/agent-channels",
            "hya/coding-extras",
            "hya/compaction",
            "hya/core-agents",
            "hya/core-commands",
            "hya/core-skills",
            "hya/hashline",
            "hya/multi-agent",
            "hya/network",
        ]
    );
    assert_eq!(inventory[0].id, "hya/agent-channels");
    assert_eq!(inventory[0].kind, "Bundle");
    assert_eq!(
        inventory[0].resource_ids,
        ["parent-dm-default", "unit-default"]
    );
    assert_eq!(inventory[1].id, "hya/coding-extras");
    assert_eq!(inventory[1].kind, "Bundle");
    assert_eq!(inventory[2].kind, "Bundle");
    assert_eq!(inventory[3].kind, "Bundle");
    assert_eq!(inventory[4].resource_ids, ["init", "review"]);
    assert_eq!(inventory[4].kind, "Bundle");
    assert_eq!(inventory[5].kind, "Bundle");
    assert_eq!(
        inventory[5].resource_ids,
        ["agent-bundle-authoring", "secure-self-update"]
    );
    assert!(inventory[6..].iter().all(|item| item.kind == "Bundle"));
    for preset in inventory {
        assert!(preset.immutable);
        assert!(!preset.installable);
        assert_eq!(preset.digest.len(), 64);
        assert_eq!(preset.version, hya_version::BACKEND_VERSION);
    }

    // `hya/core-agents` reaches the roster through the bundle catalog like any
    // agent bundle: its declared default agent is the catalog's default.
    let public = hya_app::builtin_agent_catalog().expect("public catalog");
    assert!(
        public
            .bundles()
            .bundles()
            .iter()
            .any(|bundle| bundle.identity().id == "hya/core-agents")
    );
    assert_eq!(public.default_agent_id(), "hya-main");

    // Bare profile omits all built-in presets cleanly; selective enable/disable filters without panicking.
    let bare = hya_app::trusted_preset_inventory_for(&hya_bundle::BuiltinBundleSelection::bare())
        .expect("bare inventory");
    assert!(bare.is_empty());

    let selective = hya_app::trusted_preset_inventory_for(&hya_bundle::BuiltinBundleSelection {
        enable: vec![
            "hya/coding-extras".to_string(),
            "hya/core-skills".to_string(),
            "hya/unknown-preset".to_string(),
        ],
        disable: vec!["hya/core-skills".to_string()],
        ..hya_bundle::BuiltinBundleSelection::bare()
    })
    .expect("selective inventory");
    assert_eq!(
        selective
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["hya/coding-extras"]
    );
}
