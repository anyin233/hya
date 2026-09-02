//! Integration contracts for the non-publishing release rehearsal command.

#![allow(clippy::expect_used, dead_code)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TARGET: &str = "x86_64-unknown-linux-gnu";
const WORKFLOW_CONTRACTS: &[&str] = &[
    "cp -R packages/hya-tui-ts/src/. \"$runtime/src/\"",
    "test -f \"$runtime/src/hya/coding-tool-presentation.tsx\"",
    "grep -Fx \"$package_dir/lib/hya/hya-tui-ts/src/hya/coding-tool-presentation.tsx\" \"$scratch/archive.txt\"",
    "cp packages/hya-tui-ts/NOTICE \"$runtime/NOTICE\"",
    "test -f \"$runtime/NOTICE\"",
    "cmp packages/hya-tui-ts/NOTICE \"$runtime/NOTICE\"",
    "grep -Fx \"$package_dir/lib/hya/hya-tui-ts/NOTICE\" \"$scratch/archive.txt\"",
    "cp THIRD_PARTY_NOTICES \"$runtime/THIRD_PARTY_NOTICES\"",
    "test -f \"$runtime/THIRD_PARTY_NOTICES\"",
    "cmp THIRD_PARTY_NOTICES \"$runtime/THIRD_PARTY_NOTICES\"",
    "grep -Fx \"$package_dir/lib/hya/hya-tui-ts/THIRD_PARTY_NOTICES\" \"$scratch/archive.txt\"",
    "test -d \"$runtime/node_modules\"",
    "grep -F \"$package_dir/lib/hya/hya-tui-ts/node_modules/\" \"$scratch/archive.txt\" >/dev/null",
];

/// Require an explicit no-publish guard before a rehearsal can run.
#[test]
fn release_rehearsal_requires_no_publish() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "release-rehearsal",
            "--workflow",
            ".github/workflows/release.yml",
            "--version",
            "0.36.9",
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

    let output = run_rehearsal(&workflow, "0.36.9");
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

    let output = run_rehearsal(&workflow, "0.36.9");
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

    let output = run_rehearsal(&workflow, "0.36.9");
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

/// Require every independent packaging contract omission to fail before build/publish.
#[test]
fn release_rehearsal_rejects_each_missing_package_contract() {
    let source = canonical_workflow_fixture();
    for (index, &marker) in WORKFLOW_CONTRACTS.iter().enumerate() {
        let modified = source.replacen(marker, "", 1);
        assert_ne!(
            modified, source,
            "workflow fixture did not contain contract marker `{marker}`"
        );
        let fixture = WorkflowFixture::new(&format!("release-package-contract-{index}"), modified);
        let output = run_rehearsal(fixture.path(), "0.36.9");
        assert_contract_failure(&output, marker);
    }
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

/// Hold one temporary workflow fixture and remove it when the test ends.
struct WorkflowFixture {
    directory: PathBuf,
    workflow: PathBuf,
}

impl WorkflowFixture {
    /// Write one workflow source into a temporary fixture directory.
    fn new(label: &str, source: String) -> Self {
        let directory = common::tempdir(label);
        let workflow = directory.join("release.yml");
        fs::write(&workflow, source).expect("write release workflow fixture");
        Self {
            directory,
            workflow,
        }
    }

    /// Return the fixture workflow path.
    fn path(&self) -> &Path {
        &self.workflow
    }
}

impl Drop for WorkflowFixture {
    /// Remove the temporary workflow directory even when a test assertion fails.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Return the checked-in workflow after asserting every exact package marker.
fn canonical_workflow_fixture() -> String {
    let source = fs::read_to_string(workspace_root().join(".github/workflows/release.yml"))
        .expect("read release workflow fixture");
    for &marker in WORKFLOW_CONTRACTS {
        assert!(
            source.contains(marker),
            "checked-in release workflow is missing `{marker}`"
        );
    }
    source
}

/// Assert that a workflow fixture fails before publishing and names its missing contract.
fn assert_contract_failure(output: &Output, marker: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "contract unexpectedly passed: {stderr}"
    );
    assert!(stderr.contains(marker), "stderr was: {stderr}");
}
