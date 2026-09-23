//! Release staging of the twelve trusted first-party bundle packages.

#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

mod common;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const TARGET: &str = "x86_64-unknown-linux-gnu";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const NATIVE_FAMILIES: [&str; 5] = [
    "base-tools",
    "extended-tools",
    "network-tools",
    "channel-tools",
    "todo-tools",
];
const DATA_BUNDLES: [&str; 7] = [
    "core-skills",
    "core-commands",
    "core-agents",
    "agent-channels",
    "goal-loop",
    "plan-impl-review",
    "subagents",
];

/// Write placeholder libraries named like a Linux release build.
fn fake_libraries(directory: &Path) {
    fs::create_dir_all(directory).unwrap();
    for family in NATIVE_FAMILIES {
        let name = format!("libhya_{}.so", family.replace('-', "_"));
        fs::write(directory.join(name), format!("library:{family}")).unwrap();
    }
}

fn stage(root: &Path, version: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "stage-first-party-bundles",
            "--target",
            TARGET,
            "--version",
            version,
        ])
        .arg("--library-dir")
        .arg(root.join("libraries"))
        .arg("--package-root")
        .arg(root.join("package"))
        .arg("--assets")
        .arg(root.join("dist"))
        .output()
        .expect("run xtask")
}

#[test]
fn stages_installed_packages_and_versioned_release_assets() {
    let root = common::tempdir("first-party-release");
    fake_libraries(&root.join("libraries"));
    let output = stage(&root, VERSION);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut assets = fs::read_dir(root.join("dist"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    assets.sort();
    let mut expected = NATIVE_FAMILIES
        .iter()
        .map(|family| format!("hya-{family}-{VERSION}-{TARGET}.hyabundle"))
        .chain(
            DATA_BUNDLES
                .iter()
                .map(|bundle| format!("hya-{bundle}-{VERSION}.hyabundle")),
        )
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(assets, expected);

    for name in NATIVE_FAMILIES.iter().chain(DATA_BUNDLES.iter()) {
        let installed = root
            .join("package/bundles")
            .join(format!("hya-{name}.hyabundle"));
        let bytes = fs::read(&installed).expect("installed package");
        let asset = assets
            .iter()
            .find(|asset| asset.starts_with(&format!("hya-{name}-{VERSION}")))
            .unwrap();
        assert_eq!(
            fs::read(root.join("dist").join(asset)).unwrap(),
            bytes,
            "{name}"
        );
        let catalog = hya_bundle::inspect_public_package(&bytes).expect("valid package");
        let bundle = &catalog.bundles()[0];
        assert_eq!(bundle.identity().id, format!("hya/{name}"));
        assert_eq!(bundle.identity().version, VERSION);
    }
    let base = hya_bundle::inspect_public_package(
        &fs::read(root.join("package/bundles/hya-base-tools.hyabundle")).unwrap(),
    )
    .unwrap();
    let library = base.bundles()[0]
        .extensions()
        .iter()
        .find(|asset| asset.local_id == "runtime")
        .expect("native library resource");
    assert_eq!(library.source_bytes().unwrap(), b"library:base-tools");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn refuses_a_release_version_the_bundles_do_not_carry() {
    let root = common::tempdir("first-party-release-mismatch");
    fake_libraries(&root.join("libraries"));
    let output = stage(&root, "0.0.1");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "mismatch passed: {stderr}");
    assert!(stderr.contains("0.0.1"), "stderr was: {stderr}");
    fs::remove_dir_all(root).unwrap();
}
