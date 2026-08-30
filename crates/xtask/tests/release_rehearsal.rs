//! Integration contracts for the non-publishing release rehearsal command.

#![allow(clippy::expect_used, dead_code)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TARGET: &str = "x86_64-unknown-linux-gnu";

/// Require an explicit no-publish guard before a rehearsal can run.
#[test]
fn release_rehearsal_requires_no_publish() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "release-rehearsal",
            "--workflow",
            ".github/workflows/release.yml",
            "--version",
            "0.36.7",
            "--target",
            TARGET,
        ])
        .output()
        .expect("run release rehearsal");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "rehearsal unexpectedly succeeded: {stderr}"
    );
    assert!(stderr.contains("--no-publish"), "stderr was: {stderr}");
}

/// Reject a release workflow action that uses a mutable tag instead of a SHA.
#[test]
fn release_rehearsal_rejects_mutable_action_pin() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join(".github/workflows/release.yml"))
        .expect("read release workflow fixture");
    let modified = source.replacen(
        "actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683",
        "actions/checkout@v4",
        1,
    );
    let directory = common::tempdir("release-action-pin");
    let workflow = directory.join("release.yml");
    fs::write(&workflow, modified).expect("write mutable action fixture");

    let output = run_rehearsal(&workflow, "0.36.7");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "mutable action unexpectedly passed: {stderr}"
    );
    assert!(
        stderr.contains("40-character commit SHA"),
        "stderr was: {stderr}"
    );
    fs::remove_dir_all(directory).expect("remove action pin fixture");
}

/// Require the publishing job to use the protected `release` environment.
#[test]
fn release_rehearsal_rejects_wrong_release_environment() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join(".github/workflows/release.yml"))
        .expect("read release workflow fixture");
    let modified = source.replacen("environment: release", "environment: staging", 1);
    let directory = common::tempdir("release-environment");
    let workflow = directory.join("release.yml");
    fs::write(&workflow, modified).expect("write environment fixture");

    let output = run_rehearsal(&workflow, "0.36.7");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "wrong environment unexpectedly passed: {stderr}"
    );
    assert!(
        stderr.contains("release job environment"),
        "stderr was: {stderr}"
    );
    fs::remove_dir_all(directory).expect("remove environment fixture");
}

/// Reject a workflow tag trigger that cannot admit the requested release tag.
#[test]
fn release_rehearsal_rejects_nonmatching_tag_trigger() {
    let root = workspace_root();
    let source = fs::read_to_string(root.join(".github/workflows/release.yml"))
        .expect("read release workflow fixture");
    let modified = source.replacen("- \"v*.*.*\"", "- \"release-*\"", 1);
    let directory = common::tempdir("release-tag-trigger");
    let workflow = directory.join("release.yml");
    fs::write(&workflow, modified).expect("write tag trigger fixture");

    let output = run_rehearsal(&workflow, "0.36.7");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "wrong tag trigger unexpectedly passed: {stderr}"
    );
    assert!(stderr.contains("on.push.tags"), "stderr was: {stderr}");
    fs::remove_dir_all(directory).expect("remove tag trigger fixture");
}

/// Validate requested versions before the expensive build and package stages.
#[test]
fn release_rehearsal_rejects_non_semver_version() {
    let workflow = workspace_root().join(".github/workflows/release.yml");
    let output = run_rehearsal(&workflow, "not-a-version");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "invalid version unexpectedly passed: {stderr}"
    );
    assert!(stderr.contains("semver-shaped"), "stderr was: {stderr}");
}

/// Return the repository root used by the xtask test binary.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("resolve workspace root")
        .to_path_buf()
}

/// Run the no-publish command against one workflow fixture.
fn run_rehearsal(workflow: &Path, version: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "release-rehearsal",
            "--workflow",
            workflow.to_str().expect("workflow path is UTF-8"),
            "--version",
            version,
            "--target",
            TARGET,
            "--no-publish",
        ])
        .output()
        .expect("run release rehearsal fixture")
}
