#![allow(clippy::unwrap_used, clippy::expect_used)]
//! P8 residual fault / ownership / freeze matrix (deterministic, no real disk-full).

use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use hya_updater::{
    AcceptedFloor, ApplyOptions, ArtifactDigest, ReleaseMetadata, SUPPORTED_PROTOCOL_VERSION,
    TrustRoot, UpdaterError, apply_update, commit_activation, discard_staged_release,
    journal_prepare, layout, read_selector, recover_activation, smoke_staged_release,
    stage_verified_release, verify_release_metadata, write_trust_roots,
};
use sha2::{Digest, Sha256};

fn tempdir(prefix: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "hya-updater-fault-{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn sign(signing: &SigningKey, metadata: &mut ReleaseMetadata) {
    let payload = hya_updater::canonical_metadata_payload(metadata).unwrap();
    metadata.signature = signing.sign(&payload).to_bytes().to_vec();
}

fn signed_release(
    signing: &SigningKey,
    sequence: u64,
    name: &str,
    bytes: &[u8],
) -> (ReleaseMetadata, TrustRoot) {
    let digest = Sha256::digest(bytes);
    let mut metadata = ReleaseMetadata {
        sequence,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        artifacts: vec![ArtifactDigest {
            name: name.to_string(),
            size: bytes.len() as u64,
            sha256_hex: digest.iter().map(|b| format!("{b:02x}")).collect(),
        }],
        not_before: 0,
        not_after: i64::MAX,
        recovery: false,
        protocol_version: SUPPORTED_PROTOCOL_VERSION,
        min_updater_version: "0.34.0".to_string(),
        key_id: "ci".to_string(),
        signature: Vec::new(),
    };
    sign(signing, &mut metadata);
    (
        metadata,
        TrustRoot {
            key_id: "ci".to_string(),
            verifying_key: signing.verifying_key().to_bytes(),
        },
    )
}

fn stage_seq(root: &Path, sequence: u64, floor: u64, bytes: &[u8], name: &str) {
    let signing = SigningKey::from_bytes(&[3u8; 32]);
    let (metadata, trust) = signed_release(&signing, sequence, name, bytes);
    let verified = verify_release_metadata(
        &metadata,
        &[trust],
        &AcceptedFloor { sequence: floor },
        100,
        "x86_64-unknown-linux-gnu",
    )
    .unwrap();
    stage_verified_release(root, &verified, &[(name.to_string(), bytes.to_vec())]).unwrap();
}

#[test]
fn tampered_metadata_after_sign_is_rejected() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let (mut metadata, _) = signed_release(&signing, 5, "hya-backend", b"bytes");
    // Replay/tamper: change platform after signature without resigning.
    metadata.platform = "aarch64-unknown-linux-gnu".to_string();
    let err = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 1 },
        100,
        "aarch64-unknown-linux-gnu",
    )
    .expect_err("tamper after sign must fail signature check");
    assert_eq!(err, UpdaterError::InvalidSignature);
}

#[test]
fn path_escape_artifact_name_is_rejected_at_stage() {
    let root = tempdir("escape");
    let signing = SigningKey::from_bytes(&[3u8; 32]);
    let bytes = b"evil";
    let digest = Sha256::digest(bytes);
    let mut metadata = ReleaseMetadata {
        sequence: 1,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        artifacts: vec![ArtifactDigest {
            name: "../accepted_floor".to_string(),
            size: bytes.len() as u64,
            sha256_hex: digest.iter().map(|b| format!("{b:02x}")).collect(),
        }],
        not_before: 0,
        not_after: i64::MAX,
        recovery: false,
        protocol_version: SUPPORTED_PROTOCOL_VERSION,
        min_updater_version: "0.34.0".to_string(),
        key_id: "ci".to_string(),
        signature: Vec::new(),
    };
    sign(&signing, &mut metadata);
    let trust = TrustRoot {
        key_id: "ci".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    };
    let verified = verify_release_metadata(
        &metadata,
        &[trust],
        &AcceptedFloor { sequence: 0 },
        100,
        "x86_64-unknown-linux-gnu",
    )
    .unwrap();
    let err = stage_verified_release(
        &root,
        &verified,
        &[("../accepted_floor".to_string(), bytes.to_vec())],
    )
    .expect_err("path escape must fail closed");
    assert!(matches!(err, UpdaterError::InvalidMetadata(_)));
    assert!(!root.join("accepted_floor").exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn failed_smoke_blocks_activation_and_allows_discard() {
    let root = tempdir("smoke-fail");
    let package = tempdir("pkg-smoke-fail");
    let signing = SigningKey::from_bytes(&[3u8; 32]);
    // Failing smoke script.
    let script = b"#!/bin/sh\nexit 7\n";
    std::fs::write(package.join("smoke.sh"), script).unwrap();
    let (metadata, trust) = signed_release(&signing, 1, "smoke.sh", script);
    write_trust_roots(&layout(&root).trust_roots, &[trust]).unwrap();

    let err = apply_update(ApplyOptions {
        updater_root: &root,
        metadata: &metadata,
        package_source: package.to_str().unwrap(),
        trust_roots: None,
        host_platform: "x86_64-unknown-linux-gnu",
        now_unix: 100,
        smoke_command: Some("smoke.sh"),
        smoke_args: &[],
        owner_authorized: true,
    })
    .expect_err("failed smoke must abort apply before activation");
    assert!(matches!(err, UpdaterError::SmokeFailed(_)));
    // Floor must not advance even if owner_authorized was requested.
    assert_eq!(read_selector(&root).unwrap().accepted_floor, 0);
    assert_eq!(read_selector(&root).unwrap().current_sequence, 0);
    // Candidate was staged before smoke; discard without advancing floor.
    discard_staged_release(&root, 1).unwrap();
    assert!(!root.join("releases/1").exists());

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&package).ok();
}

#[test]
fn prepare_only_recover_keeps_previous_and_floor() {
    let root = tempdir("prepare-only");
    stage_seq(&root, 1, 0, b"v1", "hya-backend");
    journal_prepare(&root, 1, 0).unwrap();
    commit_activation(&root, 1).unwrap();

    stage_seq(&root, 2, 1, b"v2", "hya-backend");
    journal_prepare(&root, 2, 1).unwrap();
    let recovered = recover_activation(&root).unwrap();
    assert_eq!(recovered.current_sequence, 1);
    assert_eq!(recovered.accepted_floor, 1);
    assert!(commit_activation(&root, 1).is_err());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn selector_without_floor_recover_finishes_commit() {
    let root = tempdir("sel-no-floor");
    stage_seq(&root, 1, 0, b"v1", "hya-backend");
    journal_prepare(&root, 1, 0).unwrap();
    commit_activation(&root, 1).unwrap();
    stage_seq(&root, 2, 1, b"v2", "hya-backend");
    journal_prepare(&root, 2, 1).unwrap();
    std::fs::write(root.join("current"), "2\n").unwrap();
    let recovered = recover_activation(&root).unwrap();
    assert_eq!(recovered.current_sequence, 2);
    assert_eq!(recovered.accepted_floor, 2);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn failed_smoke_helper_rejects_without_selector_change() {
    let root = tempdir("smoke-helper");
    let script = b"#!/bin/sh\nexit 1\n";
    stage_seq(&root, 1, 0, script, "smoke.sh");
    let staged = hya_updater::StagedRelease {
        sequence: 1,
        root: root.clone(),
    };
    let err = smoke_staged_release(&staged, "smoke.sh", &[]).unwrap_err();
    assert!(matches!(err, UpdaterError::SmokeFailed(_)));
    assert_eq!(read_selector(&root).unwrap().current_sequence, 0);
    std::fs::remove_dir_all(&root).ok();
}
