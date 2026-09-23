#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Break-glass installer remains the manual recovery path.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn install_sh_is_present_and_parses() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let install = manifest_dir
        .join("../../install.sh")
        .canonicalize()
        .expect("install.sh must exist at repo root as break-glass recovery");
    assert!(install.is_file(), "missing {}", install.display());
    let status = Command::new("bash")
        .arg("-n")
        .arg(&install)
        .status()
        .expect("spawn bash -n");
    assert!(status.success(), "install.sh must parse under bash -n");
    let body = std::fs::read_to_string(&install).unwrap();
    assert!(
        body.contains("restore_install"),
        "install.sh must retain restore_install break-glass rollback"
    );
}

fn install_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../install.sh")
        .canonicalize()
        .expect("install.sh must exist at repo root")
}

#[test]
fn install_sh_stages_every_first_party_bundle_beside_bin() {
    let prefix = std::env::temp_dir().join(format!("hya-install-dry-run-{}", std::process::id()));
    let output = Command::new("bash")
        .arg(install_script())
        .args(["--dry-run", "--prefix"])
        .arg(&prefix)
        .output()
        .expect("run install.sh --dry-run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for family in [
        "hya-base-tools",
        "hya-extended-tools",
        "hya-network-tools",
        "hya-channel-tools",
        "hya-todo-tools",
    ] {
        assert!(stdout.contains(&format!("-p {family}")), "{stdout}");
    }
    assert!(stdout.contains("stage-first-party-bundles"), "{stdout}");
    assert!(
        stdout.contains(&format!("{}/bundles", prefix.display())),
        "{stdout}"
    );
    assert!(stdout.contains("bundle list"), "{stdout}");
    assert!(
        !prefix.exists(),
        "dry run must not create {}",
        prefix.display()
    );
}

#[test]
fn install_sh_rejects_a_bin_dir_the_backend_cannot_find_bundles_from() {
    let output = Command::new("bash")
        .arg(install_script())
        .args(["--dry-run", "--bin-dir", "/tmp/hya-tools"])
        .output()
        .expect("run install.sh");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("bin"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn install_sh_does_not_reference_removed_components() {
    let body = std::fs::read_to_string(install_script()).unwrap();
    assert!(
        !body.contains("hya-plugin-compat"),
        "compat adapter crate was removed"
    );
}
