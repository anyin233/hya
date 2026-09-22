//! Ownership contract for the extended built-in tool family.

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
        "kill",
        "plan_exit",
    ] {
        assert_eq!(
            registry.builtin_bundle_origin(name),
            Some("hya/extended-tools"),
            "{name} must be owned by the extended bundle",
        );
    }
}
