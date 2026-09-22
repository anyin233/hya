//! Trusted embedded presets are visible without becoming installable bundles.

#![allow(clippy::expect_used)]

use sha2::{Digest, Sha256};

#[test]
fn builtin_skill_catalog_comes_from_prepared_core_skills_bundle() {
    let bytes = hya_tool::core_skills_preset_bytes();
    let digest = format!("{:x}", Sha256::digest(bytes));
    let catalog = hya_bundle::PreparedCatalog::decode(bytes, &digest).expect("core Skill catalog");
    let bundle = &catalog.bundles()[0];
    let builtins = hya_tool::builtin_skills();
    assert_eq!(bundle.identity().id, "hya/core-skills");
    assert_eq!(bundle.skills().len(), builtins.len());
    for (resource, builtin) in bundle.skills().iter().zip(builtins) {
        let parsed = hya_tool::parse_skill(&resource.content).expect("valid bundled Skill");
        assert_eq!(resource.local_id, builtin.name);
        assert_eq!(parsed.name, builtin.name);
        assert_eq!(parsed.description, builtin.description);
        assert_eq!(parsed.content, builtin.content);
        assert!(builtin.path.to_string_lossy().contains("hya/core-skills"));
    }
}

#[test]
fn inventory_exposes_immutable_noninstallable_core_and_tool_presets() {
    let inventory = hya_app::trusted_preset_inventory().expect("trusted preset inventory");
    assert_eq!(inventory.len(), 8);
    assert_eq!(
        inventory
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        [
            "hya/agent-channels",
            "hya/base-tools",
            "hya/channel-tools",
            "hya/core-agents",
            "hya/core-skills",
            "hya/extended-tools",
            "hya/network-tools",
            "hya/todo-tools",
        ]
    );
    assert_eq!(inventory[0].id, "hya/agent-channels");
    assert_eq!(inventory[0].kind, "AgentSetBundle");
    assert_eq!(
        inventory[0].resource_ids,
        ["parent-dm-default", "unit-default"]
    );
    assert_eq!(inventory[1].id, "hya/base-tools");
    assert_eq!(inventory[1].kind, "Plugin");
    assert_eq!(inventory[2].kind, "Plugin");
    assert_eq!(inventory[3].kind, "AgentSetBundle");
    assert_eq!(inventory[4].kind, "Plugin");
    assert_eq!(
        inventory[4].resource_ids,
        ["agent-bundle-authoring", "secure-self-update"]
    );
    assert!(inventory[5..].iter().all(|item| item.kind == "Plugin"));
    for preset in inventory {
        assert!(preset.immutable);
        assert!(!preset.installable);
        assert_eq!(preset.digest.len(), 64);
        assert_eq!(preset.version, "1.0.0");
    }

    let public = hya_app::builtin_agent_catalog().expect("public catalog");
    assert!(
        public
            .bundles()
            .bundles()
            .iter()
            .all(|bundle| bundle.identity().id != "hya/core-agents"),
        "trusted presets must not enter the installable BundleCatalog"
    );
}
