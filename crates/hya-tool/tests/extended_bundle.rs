//! Ownership contract for the extended built-in tool family.
#![allow(clippy::expect_used)]

use hya_tool::ToolRegistry;

#[test]
fn extended_tools_are_registered_from_their_native_bundle() {
    let registry = ToolRegistry::builtins();
    for name in [
        "invalid",
        "lsp",
        "skill",
        "list_agents",
        "task",
        "workflow",
        "search_agent",
        "archive",
        "plan_exit",
    ] {
        assert_eq!(
            registry.builtin_bundle_origin(name),
            Some("hya/extended-tools"),
            "{name} must be owned by the extended bundle",
        );
    }
}

/// `kill` was removed in 0.41.0; `archive` (stop + archive, wakeable by mail)
/// replaces it. No hidden alias keeps the old spelling alive.
#[test]
fn kill_is_no_longer_a_registered_tool() {
    let registry = ToolRegistry::builtins();
    assert!(registry.resolve("kill").is_none());
    let archive = registry.resolve("archive").expect("archive is registered");
    assert_eq!(archive.permission, hya_tool::ToolPermission::Task);
    let schema = archive.tool.schema();
    let text = serde_json::to_string(&schema).unwrap_or_default();
    assert!(
        text.contains("\"target\"") && text.contains("wakes it again"),
        "archive schema names the target and says archived agents can be woken: {text}"
    );
}
