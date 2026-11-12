//! Integration contracts for the non-publishing release rehearsal command.

#![allow(clippy::expect_used)]

use std::process::{Command, Output};

/// Reject a target that differs from the host before any build starts.
#[test]
fn release_rehearsal_requires_a_host_target() {
    let host = host_target();
    let foreign = [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "aarch64-apple-darwin",
    ]
    .into_iter()
    .find(|target| *target != host)
    .expect("a release target differs from the host");
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["release-rehearsal"])
        .args([
            "--version",
            hya_version::BACKEND_VERSION,
            "--target",
            foreign,
            "--no-publish",
        ])
        .output()
        .expect("run release rehearsal");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "foreign target passed: {stderr}");
    assert!(stderr.contains("host"), "stderr was: {stderr}");
}

/// Require an explicit no-publish guard before a rehearsal can run.
#[test]
fn release_rehearsal_requires_no_publish() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["release-rehearsal", "--version", "0.36.9", "--target"])
        .arg(host_target())
        .output()
        .expect("run release rehearsal");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "rehearsal unexpectedly succeeded: {stderr}"
    );
    assert!(stderr.contains("--no-publish"), "stderr was: {stderr}");
}

/// Validate requested versions before the expensive build and package stages.
#[test]
fn release_rehearsal_rejects_non_semver_version() {
    let output = run_rehearsal("not-a-version");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "invalid version unexpectedly passed: {stderr}"
    );
    assert!(stderr.contains("semver-shaped"), "stderr was: {stderr}");
}

fn host_target() -> String {
    String::from_utf8(
        Command::new("rustc")
            .arg("-vV")
            .output()
            .expect("run rustc")
            .stdout,
    )
    .expect("rustc output is UTF-8")
    .lines()
    .find_map(|line| line.strip_prefix("host: "))
    .expect("rustc reports its host")
    .to_owned()
}

/// Run the no-publish command for `version` on the host target.
fn run_rehearsal(version: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["release-rehearsal", "--version", version, "--target"])
        .arg(host_target())
        .arg("--no-publish")
        .output()
        .expect("run release rehearsal")
}
