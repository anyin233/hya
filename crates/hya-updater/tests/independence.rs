#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The updater TCB must stay free of runtime extension authorities.
#[test]
fn cargo_toml_has_no_runtime_extension_dependencies() {
    let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    for forbidden in [
        "hya-core",
        "hya-app",
        "hya-plugin",
        "hya-mcp",
        "hya-bundle",
        "hya-server",
        "hya-store",
        "hya-tool",
        "hya-provider",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "hya-updater must not depend on `{forbidden}`"
        );
    }
}
