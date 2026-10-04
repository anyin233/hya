//! Bundle-owned TUI manifest validation and canonical preparation.

#![allow(clippy::expect_used, clippy::unwrap_used)]
use hya_bundle::{BundleError, BundleSource, SourceFile, prepare_package};

fn tui_bundle(tui: &str, entry_path: &str, entry_bytes: &[u8]) -> BundleSource {
    let manifest = format!(
        "kind: AgentBundle\nidentity:\n  id: hya/tui-test\n  version: 1.0.0\n  publisher: hya\nagent:\n  id: tui-test\n  role: main\n  prompt: prompts/lead.md\nextensions:\n  js:\n    - id: entry\n      path: {entry_path}\n  process:\n    kind: bun\n    command: [bun, run, '{entry_path}']\ntui:\n{tui}"
    );
    BundleSource::new(
        "tui-test",
        vec![
            SourceFile::new("bundle.yaml", manifest.into_bytes()),
            SourceFile::new("prompts/lead.md", b"TUI test agent\n"),
            SourceFile::new(entry_path, entry_bytes),
        ],
    )
}

fn valid_tui(entry: &str, sdk: &str, permissions: &str) -> String {
    format!("  api_version: 1\n  entry: {entry}\n  sdk: {sdk}\n  permissions: {permissions}\n")
}

fn invalid_detail(source: BundleSource) -> String {
    match prepare_package(source) {
        Err(BundleError::InvalidManifest { detail, .. }) => detail,
        Err(error) => panic!("expected invalid manifest, got {error:?}"),
        Ok(_) => panic!("manifest unexpectedly prepared"),
    }
}

#[test]
fn tui_accepts_supported_api_and_sdk_versions() {
    for sdk in ["1.0", "1.2.3"] {
        let prepared = prepare_package(tui_bundle(
            &valid_tui("entry.ts", sdk, "[tui.panel]"),
            "entry.ts",
            b"export default {};",
        ))
        .expect("supported TUI declaration should prepare");
        assert!(prepared.bundles()[0].tui().is_some());
    }
}

#[test]
fn tui_rejects_unsupported_api_and_sdk_versions() {
    for tui in [
        valid_tui("entry.ts", "1.2.3", "[]").replacen("api_version: 1", "api_version: 2", 1),
        valid_tui("entry.ts", "2.0", "[]"),
    ] {
        let detail = invalid_detail(tui_bundle(&tui, "entry.ts", b"export default {};"));
        assert!(detail.contains("tui."), "diagnostic: {detail}");
    }
}

#[test]
fn tui_requires_canonical_ts_js_entry_resource() {
    let cases = [("missing.ts", "entry.ts"), ("entry.txt", "entry.txt")];
    for (declared, file) in cases {
        let tui = valid_tui(declared, "1.0", "[]");
        let detail = invalid_detail(tui_bundle(&tui, file, b"export default {};"));
        assert!(detail.contains("tui.entry"), "diagnostic: {detail}");
    }
    let detail = invalid_detail(tui_bundle(
        &valid_tui("entry.ts", "1.0", "[]"),
        "other.ts",
        b"export default {};",
    ));
    assert!(detail.contains("tui.entry"));
}

#[test]
fn tui_permissions_are_allowlisted_unique_and_canonicalized() {
    let prepared = prepare_package(tui_bundle(
        &valid_tui("entry.ts", "1.0", "[tui.render, tui.panel]"),
        "entry.ts",
        b"export default {};",
    ))
    .expect("allowlisted permissions should prepare");
    let tui = prepared.bundles()[0].tui().expect("TUI declaration");
    assert_eq!(tui.permissions, ["tui.panel", "tui.render"]);

    for permissions in ["[tui.unknown]", "[tui.panel, tui.panel]"] {
        let detail = invalid_detail(tui_bundle(
            &valid_tui("entry.ts", "1.0", permissions),
            "entry.ts",
            b"export default {};",
        ));
        assert!(detail.contains("tui.permissions"), "diagnostic: {detail}");
    }
}

#[test]
fn tui_entry_and_permissions_are_canonicalized() {
    let prepared = prepare_package(tui_bundle(
        &valid_tui("./entry.ts", "1.0", "[tui.render, tui.panel]"),
        "entry.ts",
        b"export default {};",
    ))
    .expect("non-canonical source declaration should normalize");
    let tui = prepared.bundles()[0].tui().expect("TUI declaration");
    assert_eq!(tui.entry, "entry.ts");
    assert_eq!(tui.permissions, ["tui.panel", "tui.render"]);
}

#[test]
fn tui_is_absent_without_declaration_for_compatibility() {
    let prepared = prepare_package(tui_bundle("", "entry.ts", b"export default {};"))
        .expect("manifest without tui should remain compatible");
    assert!(prepared.bundles()[0].tui().is_none());
}
