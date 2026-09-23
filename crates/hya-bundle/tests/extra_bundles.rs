//! `hya-extra/*` distribution bundles: every source directory under
//! `bundles/extra/` must prepare cleanly, follow the extras identity rules
//! (`hya-extra/<name>` id, `hya-extra` publisher, workspace version), and
//! package deterministically through the canonical writer.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use hya_bundle::{
    BundleSource, PreparedBundleKind, first_party_source_root, inspect_public_package,
    prepare_package, write_public_package,
};

fn extra_bundle_dirs() -> Vec<PathBuf> {
    let root = first_party_source_root().join("extra");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

#[test]
fn every_extra_bundle_prepares_and_packages_deterministically() {
    let dirs = extra_bundle_dirs();
    assert!(
        !dirs.is_empty(),
        "expected at least one bundles/extra/* source directory"
    );

    for dir in &dirs {
        let source = BundleSource::read_directory(dir)
            .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));

        let prepared = prepare_package(source.clone())
            .unwrap_or_else(|error| panic!("prepare {}: {error}", dir.display()));
        assert_eq!(
            prepared.bundles().len(),
            1,
            "expected exactly one bundle in {}",
            dir.display()
        );
        let bundle = &prepared.bundles()[0];
        let identity = bundle.identity();
        assert!(
            identity.id.starts_with("hya-extra/"),
            "{}: identity id {} must start with hya-extra/",
            dir.display(),
            identity.id
        );
        assert_eq!(
            identity.publisher,
            "hya-extra",
            "{}: publisher must be hya-extra",
            dir.display()
        );
        assert_eq!(
            identity.version,
            env!("CARGO_PKG_VERSION"),
            "{}: extras version with hya (identity version must equal workspace version)",
            dir.display()
        );

        // Canonical package writer output is deterministic: packaging the same
        // source twice must produce byte-identical archives.
        let first = write_public_package(&source)
            .unwrap_or_else(|error| panic!("package {}: {error}", dir.display()));
        let second = write_public_package(&source)
            .unwrap_or_else(|error| panic!("package {}: {error}", dir.display()));
        assert_eq!(
            first,
            second,
            "non-deterministic package bytes for {}",
            dir.display()
        );

        // Re-preparing from the packaged archive reaches the same digest as
        // preparing directly from the source directory.
        let reprepared = inspect_public_package(&first)
            .unwrap_or_else(|error| panic!("inspect packaged {}: {error}", dir.display()));
        assert_eq!(
            reprepared.digest(),
            prepared.digest(),
            "archive round-trip digest mismatch for {}",
            dir.display()
        );
    }
}

#[test]
fn zvec_grep_is_a_plugin_with_one_mcp_server_and_one_skill() {
    let dir = first_party_source_root().join("extra/zvec-grep");
    let source = BundleSource::read_directory(&dir).expect("read zvec-grep source");
    let prepared = prepare_package(source).expect("prepare zvec-grep");
    let bundle = &prepared.bundles()[0];

    assert_eq!(bundle.kind(), PreparedBundleKind::Plugin);
    assert!(
        bundle.agents().is_empty(),
        "a Plugin bundle carries no synthetic agent"
    );
    assert_eq!(bundle.mcp().len(), 1, "expected exactly one MCP server");
    assert_eq!(bundle.skills().len(), 1, "expected exactly one Skill");
    assert_eq!(bundle.mcp()[0].local_id, "zvec-grep");
    assert_eq!(bundle.skills()[0].local_id, "zvec-grep");
}
