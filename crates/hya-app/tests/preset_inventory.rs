//! Trusted embedded presets are visible without becoming installable bundles.

#![allow(clippy::expect_used)]

#[test]
fn inventory_exposes_immutable_noninstallable_core_and_tool_presets() {
    let inventory = hya_app::trusted_preset_inventory().expect("trusted preset inventory");
    assert_eq!(inventory.len(), 3);
    assert_eq!(inventory[0].id, "hya/agent-channels");
    assert_eq!(inventory[0].kind, "AgentSetBundle");
    assert_eq!(
        inventory[0].resource_ids,
        ["parent-dm-default", "unit-default"]
    );
    assert_eq!(inventory[1].id, "hya/base-tools");
    assert_eq!(inventory[1].kind, "Plugin");
    assert_eq!(inventory[2].id, "hya/core-agents");
    assert_eq!(inventory[2].kind, "AgentSetBundle");
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
