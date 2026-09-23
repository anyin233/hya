//! Parity checks for the embedded tool-family exposure policies.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use hya_tool::{
    AliasVisibility, ToolPermission, ToolRegistry, base_tools_preset, tool_bundle_presets,
};

#[test]
fn base_bundle_owns_only_the_requested_foundational_tools() {
    let actual = base_tools_preset()
        .tools()
        .iter()
        .map(|tool| tool.name())
        .collect::<BTreeSet<_>>();
    let expected = [
        "read",
        "write",
        "edit",
        "ls",
        "glob",
        "find",
        "grep",
        "ask_user",
        "bash",
        "apply_patch",
    ]
    .into_iter()
    .collect();
    assert_eq!(actual, expected);
}

#[test]
fn each_embedded_bundle_owns_its_requested_tool_family() {
    let expected = [
        (
            "hya/base-tools",
            &[
                "read",
                "write",
                "edit",
                "ls",
                "glob",
                "find",
                "grep",
                "ask_user",
                "bash",
                "apply_patch",
            ][..],
        ),
        (
            "hya/extended-tools",
            &[
                "invalid",
                "lsp",
                "skill",
                "list_agents",
                "task",
                "workflow",
                "search_agent",
                "archive",
                "plan_exit",
                "wait",
            ],
        ),
        ("hya/network-tools", &["webfetch", "websearch"]),
        (
            "hya/channel-tools",
            &["send", "list_channel", "report", "wait"],
        ),
        (
            "hya/todo-tools",
            &["todo__read", "todo__update_status", "todo__update_content"],
        ),
    ];
    for (preset, (identity, expected_names)) in tool_bundle_presets().iter().zip(expected) {
        assert_eq!(preset.identity(), identity);
        let actual = preset
            .tools()
            .iter()
            .map(|tool| tool.name())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actual,
            expected_names.iter().copied().collect(),
            "{identity}"
        );
    }
}

#[test]
fn embedded_base_tools_preset_is_the_registry_authority() {
    let presets = tool_bundle_presets();
    assert_eq!(
        presets
            .iter()
            .map(|preset| preset.identity())
            .collect::<Vec<_>>(),
        [
            "hya/base-tools",
            "hya/extended-tools",
            "hya/network-tools",
            "hya/channel-tools",
            "hya/todo-tools",
        ]
    );
    for preset in presets {
        assert_eq!(preset.schema_version(), 1);
        assert_eq!(preset.bundle_digest().len(), 64);
        assert!(!preset.prepared_catalog_bytes().is_empty());
    }

    let frozen = [
        ("invalid", ToolPermission::Tool, &[][..]),
        ("read", ToolPermission::ReadOnly, &[][..]),
        ("write", ToolPermission::Tool, &[][..]),
        ("edit", ToolPermission::Tool, &[][..]),
        ("ls", ToolPermission::ReadOnly, &[][..]),
        ("glob", ToolPermission::ReadOnly, &[][..]),
        ("find", ToolPermission::ReadOnly, &[][..]),
        ("grep", ToolPermission::ReadOnly, &[][..]),
        ("lsp", ToolPermission::ReadOnly, &[][..]),
        ("skill", ToolPermission::ReadOnly, &[][..]),
        ("list_agents", ToolPermission::ReadOnly, &[][..]),
        ("task", ToolPermission::Task, &[][..]),
        ("workflow", ToolPermission::Tool, &[][..]),
        ("send", ToolPermission::Tool, &[][..]),
        ("list_channel", ToolPermission::ReadOnly, &[][..]),
        ("search_agent", ToolPermission::ReadOnly, &[][..]),
        ("report", ToolPermission::Tool, &[][..]),
        ("archive", ToolPermission::Task, &[][..]),
        // extended-tools' `wait` and channel-tools' mail-aware override.
        ("wait", ToolPermission::ReadOnly, &[][..]),
        ("wait", ToolPermission::ReadOnly, &[][..]),
        ("ask_user", ToolPermission::Tool, &["question"][..]),
        ("bash", ToolPermission::Command, &["shell"][..]),
        ("apply_patch", ToolPermission::Tool, &["patch"][..]),
        ("webfetch", ToolPermission::Tool, &["fetch"][..]),
        ("websearch", ToolPermission::Tool, &["search"][..]),
        ("plan_exit", ToolPermission::Tool, &["plan"][..]),
        ("todo__read", ToolPermission::ReadOnly, &[][..]),
        ("todo__update_status", ToolPermission::Tool, &[][..]),
        ("todo__update_content", ToolPermission::Tool, &[][..]),
    ];
    let mut actual_policy = presets
        .iter()
        .flat_map(|preset| preset.tools())
        .map(|tool| {
            (
                tool.name(),
                tool.permission(),
                tool.aliases()
                    .iter()
                    .map(|alias| alias.name())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    let mut expected_policy = frozen
        .iter()
        .map(|(name, permission, aliases)| (*name, *permission, aliases.to_vec()))
        .collect::<Vec<_>>();
    actual_policy.sort_by(|left, right| left.0.cmp(right.0));
    expected_policy.sort_by(|left, right| left.0.cmp(right.0));
    assert_eq!(actual_policy, expected_policy);

    let registry = ToolRegistry::builtins();
    let actual = registry
        .schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect::<BTreeSet<_>>();
    let expected = presets
        .iter()
        .flat_map(|preset| preset.tools())
        .filter(|tool| tool.exposed())
        .map(|tool| tool.name().to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);

    for tool in presets.iter().flat_map(|preset| preset.tools()) {
        assert_eq!(tool.schema_version(), 1, "{} schema version", tool.name());
        let resolved = registry
            .resolve(tool.name())
            .expect("preset tool is installed");
        assert_eq!(
            resolved.permission,
            tool.permission(),
            "{} permission",
            tool.name()
        );
        for alias in tool.aliases() {
            let resolved_alias = registry
                .resolve(alias.name())
                .expect("preset alias resolves");
            assert_eq!(resolved_alias.tool.name(), tool.name());
            if alias.visibility() == AliasVisibility::Hidden {
                assert!(
                    registry
                        .schemas()
                        .iter()
                        .all(|schema| schema.name.as_str() != alias.name())
                );
            }
        }
    }
}

#[test]
fn preset_preserves_read_protection_and_permission_defaults() {
    let preset = base_tools_preset();
    let extended = &tool_bundle_presets()[1];
    assert!(preset.is_protected("read"));
    assert!(!preset.is_protected("write"));
    assert_eq!(
        preset.tool("read").unwrap().permission(),
        ToolPermission::ReadOnly
    );
    assert_eq!(
        preset.tool("bash").unwrap().permission(),
        ToolPermission::Command
    );
    assert_eq!(
        extended.tool("task").unwrap().permission(),
        ToolPermission::Task
    );
    assert_eq!(
        preset.tool("write").unwrap().permission(),
        ToolPermission::Tool
    );
}

#[test]
fn tool_policies_and_core_skills_come_from_runtime_loaded_bundles() {
    for preset in hya_tool::tool_bundle_presets() {
        let catalog = hya_bundle::first_party_bundle(preset.identity())
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            std::ptr::eq(preset.prepared_catalog_bytes(), catalog.bytes()),
            "{} policy is not the runtime-loaded bundle",
            preset.identity()
        );
        assert_eq!(preset.bundle_digest(), catalog.bundles()[0].digest());
    }
    let skills =
        hya_bundle::first_party_bundle("hya/core-skills").unwrap_or_else(|error| panic!("{error}"));
    assert!(std::ptr::eq(
        hya_tool::core_skills_preset_bytes(),
        skills.bytes()
    ));
}
