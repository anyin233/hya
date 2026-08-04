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
