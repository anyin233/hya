//! Parity checks for the embedded `hya/base-tools` exposure policy.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use hya_tool::{AliasVisibility, ToolPermission, ToolRegistry, base_tools_preset};

#[test]
fn embedded_base_tools_preset_is_the_registry_authority() {
    let preset = base_tools_preset();
    assert_eq!(preset.identity(), "hya/base-tools");
    assert_eq!(preset.schema_version(), 1);
    assert_eq!(preset.bundle_digest().len(), 64);
    assert!(!preset.prepared_catalog_bytes().is_empty());

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
        ("kill", ToolPermission::Task, &[][..]),
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
    let actual_policy = preset
        .tools()
        .iter()
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
    let expected_policy = frozen
        .iter()
        .map(|(name, permission, aliases)| (*name, *permission, aliases.to_vec()))
        .collect::<Vec<_>>();
    assert_eq!(actual_policy, expected_policy);

    let registry = ToolRegistry::builtins();
    let actual = registry
        .schemas()
        .into_iter()
        .map(|schema| schema.name.as_str().to_string())
        .collect::<BTreeSet<_>>();
    let expected = preset
        .tools()
        .iter()
        .filter(|tool| tool.exposed())
        .map(|tool| tool.name().to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected);

    for tool in preset.tools() {
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
        preset.tool("task").unwrap().permission(),
        ToolPermission::Task
    );
    assert_eq!(
        preset.tool("write").unwrap().permission(),
        ToolPermission::Tool
    );
}
