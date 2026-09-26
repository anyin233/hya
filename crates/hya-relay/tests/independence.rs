//! The relay crate stays free of hya runtime crates: `hya proxy` links only
//! this crate, and the relay must not grow a dependency on the agent runtime.
#[test]
fn cargo_toml_has_no_hya_runtime_dependencies() {
    let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    for forbidden in [
        "hya-core",
        "hya-app",
        "hya-server",
        "hya-store",
        "hya-tool",
        "hya-provider",
        "hya-proto",
        "hya-api",
        "hya-bundle",
        "hya-plugin",
        "hya-mcp",
        "hya-workflow",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "hya-relay must not depend on {forbidden}"
        );
    }
}
