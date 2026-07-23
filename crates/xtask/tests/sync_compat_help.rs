#![allow(clippy::expect_used)]

use std::process::Command;

#[test]
fn sync_compat_help_prints_usage() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["sync-compat", "--help"])
        .output()
        .expect("run xtask help");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "expected success, stderr was: {stderr}"
    );
    assert!(stdout.contains("sync-compat"), "stdout was: {stdout}");
    assert!(stdout.contains("--dry-run"), "stdout was: {stdout}");
    assert!(stdout.contains("--prune"), "stdout was: {stdout}");
}
