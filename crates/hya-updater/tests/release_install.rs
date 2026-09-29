#![allow(clippy::expect_used)]
//! The release installer (`scripts/hya-install.sh`) that `curl … | sh` and
//! bare `hya update` run: layout, no-op, checksum, and rollback contract.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn release_installer_contract_passes() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/hya_install_script.sh")
        .canonicalize()
        .expect("tests/hya_install_script.sh must exist");
    let output = Command::new("bash")
        .arg(&script)
        .output()
        .expect("run tests/hya_install_script.sh");
    assert!(
        output.status.success(),
        "installer contract failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
