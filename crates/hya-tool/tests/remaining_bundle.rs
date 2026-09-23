//! Bundle ownership contract for the remaining built-in tools.

use hya_tool::ToolRegistry;

#[test]
fn every_remaining_builtin_comes_from_its_owned_bundle() {
    let registry = ToolRegistry::builtins();
    for (bundle, names) in [
        (
            "hya/base-tools",
            [
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
            .as_slice(),
        ),
        ("hya/network-tools", ["webfetch", "websearch"].as_slice()),
        (
            "hya/channel-tools",
            ["send", "list_channel", "report"].as_slice(),
        ),
    ] {
        for name in names {
            assert_eq!(registry.builtin_bundle_origin(name), Some(bundle), "{name}");
        }
    }
}
