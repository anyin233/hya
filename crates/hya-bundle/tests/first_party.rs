//! Runtime resolution and loading of hya's trusted first-party bundles.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use hya_bundle::{
    BundleError, BundleSource, FIRST_PARTY_BUNDLES, FirstPartySource, first_party_bundle,
    first_party_source, first_party_source_root, load_first_party, prepare_package,
    write_public_package,
};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "hya-first-party-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stage_package(directory: &Path, identity: &str, source_identity: &str) -> PathBuf {
    let name = source_identity.strip_prefix("hya/").expect("hya identity");
    let source = BundleSource::read_directory(first_party_source_root().join("presets").join(name))
        .expect("read in-tree source");
    std::fs::create_dir_all(directory).expect("create bundles directory");
    let package = directory.join(format!(
        "hya-{}.hyabundle",
        identity.strip_prefix("hya/").expect("hya identity")
    ));
    std::fs::write(
        &package,
        write_public_package(&source).expect("write public package"),
    )
    .expect("write package");
    package
}

#[test]
fn allowlist_names_every_in_tree_first_party_source() {
    for identity in FIRST_PARTY_BUNDLES {
        let source = first_party_source(Path::new("/nowhere/target/debug"), identity);
        assert!(
            matches!(source, Some(FirstPartySource::Directory(_))),
            "{identity}: {source:?}"
        );
    }
    assert!(FIRST_PARTY_BUNDLES.contains(&"hya/core-commands"));
    assert!(first_party_source(Path::new("/nowhere/target/debug"), "hya/unknown").is_none());
    assert!(first_party_source(Path::new("/nowhere/target/debug"), "acme/core-skills").is_none());
}

#[test]
fn installed_layout_loads_the_package_beside_bin() {
    let scratch = Scratch::new("installed");
    let package = stage_package(
        &scratch.0.join("bundles"),
        "hya/core-skills",
        "hya/core-skills",
    );
    let bin = scratch.0.join("bin");
    let source = first_party_source(&bin, "hya/core-skills");
    assert_eq!(source, Some(FirstPartySource::Package(package)));

    let catalog = load_first_party(&source.expect("package source"), "hya/core-skills")
        .expect("load installed package");
    let expected = prepare_package(
        BundleSource::read_directory(first_party_source_root().join("presets/core-skills"))
            .expect("read source"),
    )
    .expect("prepare source");
    assert_eq!(catalog.digest(), expected.digest());
    assert_eq!(catalog.bundles()[0].identity().id, "hya/core-skills");

    assert_eq!(
        first_party_source(&scratch.0.join("empty/bin"), "hya/core-skills"),
        None
    );
}

#[test]
fn cargo_layout_prefers_in_tree_source_over_staged_package() {
    let scratch = Scratch::new("cargo");
    let debug = scratch.0.join("target/debug");
    stage_package(&debug.join("bundles"), "hya/core-skills", "hya/core-skills");
    for directory in [debug.clone(), debug.join("deps")] {
        assert_eq!(
            first_party_source(&directory, "hya/core-skills"),
            Some(FirstPartySource::Directory(
                first_party_source_root().join("presets/core-skills")
            )),
            "{}",
            directory.display()
        );
    }
}

#[test]
fn package_with_another_identity_is_rejected() {
    let scratch = Scratch::new("mismatch");
    let package = stage_package(
        &scratch.0.join("bundles"),
        "hya/core-agents",
        "hya/core-skills",
    );
    let error = load_first_party(&FirstPartySource::Package(package), "hya/core-agents")
        .expect_err("identity mismatch must fail");
    assert!(
        matches!(error, BundleError::FirstPartyBundle { ref identity, .. } if identity == "hya/core-agents"),
        "{error}"
    );
}

#[test]
fn process_catalog_is_loaded_once() {
    let first = first_party_bundle("hya/core-skills").expect("load core skills");
    let second = first_party_bundle("hya/core-skills").expect("reuse core skills");
    assert!(std::ptr::eq(first, second));
    assert_eq!(first.bundles()[0].identity().id, "hya/core-skills");
    assert!(first_party_bundle("hya/unknown").is_err());
}

#[test]
fn no_crate_compiles_first_party_bundle_content_into_the_binary() {
    let workspace = first_party_source_root().join("..");
    let mut offenders = Vec::new();
    let mut roots = vec![workspace.join("crates")];
    roots.extend(
        std::fs::read_dir(first_party_source_root().join("presets"))
            .expect("list presets")
            .map(|entry| entry.expect("preset entry").path().join("native")),
    );
    let mut stack = roots;
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            // Test fixtures are not part of the shipped binary.
            if path
                .file_name()
                .is_some_and(|name| name == "target" || name == "tests")
            {
                continue;
            }
            stack.extend(
                std::fs::read_dir(&path)
                    .expect("read source directory")
                    .map(|entry| entry.expect("source entry").path()),
            );
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read Rust source");
        let build_script = path.file_name().is_some_and(|name| name == "build.rs");
        let embeds = text.lines().any(|line| {
            (line.contains("include_str!(") || line.contains("include_bytes!("))
                && (line.contains("bundles/") || line.contains("OUT_DIR"))
        });
        if embeds || (build_script && text.contains("bundles/")) {
            offenders.push(
                path.strip_prefix(&workspace)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
        }
    }
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "first-party bundle content must load at runtime, not compile in: {offenders:?}"
    );
}
