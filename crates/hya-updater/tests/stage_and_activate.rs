#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use hya_updater::{
    AcceptedFloor, ArtifactDigest, ReleaseMetadata, TrustRoot, UpdaterError, assert_no_session_or_secret_reads,
    assert_tcb_outside_candidate, commit_activation, journal_prepare, layout, read_selector,
    recover_activation, smoke_staged_release, stage_verified_release, verify_artifact_bytes,
    verify_release_metadata,
};
use sha2::{Digest, Sha256};

fn tempdir() -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "hya-updater-{}-{}",
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

fn verify_and_stage(
    root: &Path,
    sequence: u64,
    floor: u64,
    bytes: &[u8],
    name: &str,
) -> hya_updater::StagedRelease {
    let signing = SigningKey::from_bytes(&[3u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
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
        key_id: "ci".to_string(),
        signature: Vec::new(),
    };
    sign(&signing, &mut metadata);
    let verified = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: floor },
        100,
        "x86_64-unknown-linux-gnu",
    )
    .unwrap();
    stage_verified_release(root, &verified, &[(name.to_string(), bytes.to_vec())]).unwrap()
}

#[test]
fn stage_then_commit_advances_floor_and_selector() {
    let root = tempdir();
    let bytes = b"artifact-v1";
    let staged = verify_and_stage(&root, 1, 0, bytes, "hya-backend");
    assert!(staged.directory().join("hya-backend").is_file());

    // Immutable: restage same sequence fails.
    let signing = SigningKey::from_bytes(&[3u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let digest = Sha256::digest(bytes);
    let mut metadata = ReleaseMetadata {
        sequence: 1,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        artifacts: vec![ArtifactDigest {
            name: "hya-backend".to_string(),
            size: bytes.len() as u64,
            sha256_hex: digest.iter().map(|b| format!("{b:02x}")).collect(),
        }],
        not_before: 0,
        not_after: i64::MAX,
        recovery: false,
        key_id: "ci".to_string(),
        signature: Vec::new(),
    };
    sign(&signing, &mut metadata);
    let verified = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 0 },
        100,
        "x86_64-unknown-linux-gnu",
    )
    .unwrap();
    assert!(
        stage_verified_release(
            &root,
            &verified,
            &[("hya-backend".to_string(), bytes.to_vec())],
        )
        .is_err()
    );

    journal_prepare(&root, 1, 0).unwrap();
    let selector = commit_activation(&root, 1).unwrap();
    assert_eq!(selector.current_sequence, 1);
    assert_eq!(selector.accepted_floor, 1);
    assert_eq!(read_selector(&root).unwrap().current_sequence, 1);

    // Anti-rollback: cannot commit lower/equal sequence after floor advanced.
    assert!(commit_activation(&root, 1).is_err());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn recover_prepare_without_selector_keeps_previous_generation() {
    let root = tempdir();
    verify_and_stage(&root, 1, 0, b"v1", "hya-backend");
    journal_prepare(&root, 1, 0).unwrap();
    commit_activation(&root, 1).unwrap();

    verify_and_stage(&root, 2, 1, b"v2", "hya-backend");
    journal_prepare(&root, 2, 1).unwrap();
    // Crash before selector rename: recover must keep generation 1 and floor 1.
    let recovered = recover_activation(&root).unwrap();
    assert_eq!(recovered.current_sequence, 1);
    assert_eq!(recovered.accepted_floor, 1);
    // Floor never decrements on aborted prepare.
    assert!(commit_activation(&root, 1).is_err());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn recover_prepare_after_selector_switch_finishes_commit() {
    let root = tempdir();
    verify_and_stage(&root, 1, 0, b"v1", "hya-backend");
    journal_prepare(&root, 1, 0).unwrap();
    commit_activation(&root, 1).unwrap();

    verify_and_stage(&root, 2, 1, b"v2", "hya-backend");
    journal_prepare(&root, 2, 1).unwrap();
    // Simulate selector rename without floor/journal commit.
    std::fs::write(root.join("current"), "2\n").unwrap();
    // Floor still at 1.
    assert_eq!(read_selector(&root).unwrap().accepted_floor, 1);

    let recovered = recover_activation(&root).unwrap();
    assert_eq!(recovered.current_sequence, 2);
    assert_eq!(recovered.accepted_floor, 2);
    assert_eq!(read_selector(&root).unwrap().accepted_floor, 2);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn smoke_runs_in_dedicated_subprocess() {
    let root = tempdir();
    let script = b"#!/bin/sh\necho smoke-ok\n";
    let staged = verify_and_stage(&root, 1, 0, script, "smoke.sh");
    let path = staged.directory().join("smoke.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
    smoke_staged_release(&staged, "smoke.sh", &[]).expect("smoke must pass");
    // Path escape rejected.
    assert!(matches!(
        smoke_staged_release(&staged, "../current", &[]),
        Err(UpdaterError::SmokeFailed(_))
    ));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn ownership_layout_keeps_tcb_outside_candidate() {
    let root = tempdir();
    assert_tcb_outside_candidate(&root, 7).unwrap();
    assert_no_session_or_secret_reads(&root).unwrap();
    let layout = layout(&root);
    assert_eq!(layout.selector, root.join("current"));
    assert_eq!(layout.journal, root.join("activation.journal"));
    assert_eq!(layout.accepted_floor, root.join("accepted_floor"));
    assert_eq!(layout.trust_roots, root.join("trust_roots.json"));
    assert!(!layout.selector.starts_with(root.join("releases")));

    // Inject a forbidden session path and reject.
    std::fs::write(root.join("sessions.sqlite"), b"nope").unwrap();
    assert!(matches!(
        assert_no_session_or_secret_reads(&root),
        Err(UpdaterError::OwnershipViolation(_))
    ));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn artifact_digest_mismatch_is_rejected() {
    let signing = SigningKey::from_bytes(&[3u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let good = b"good-bytes";
    let digest = Sha256::digest(good);
    let mut metadata = ReleaseMetadata {
        sequence: 1,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        artifacts: vec![ArtifactDigest {
            name: "hya-backend".to_string(),
            size: good.len() as u64,
            sha256_hex: digest.iter().map(|b| format!("{b:02x}")).collect(),
        }],
        not_before: 0,
        not_after: i64::MAX,
        recovery: false,
        key_id: "ci".to_string(),
        signature: Vec::new(),
    };
    sign(&signing, &mut metadata);
    let verified = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 0 },
        100,
        "x86_64-unknown-linux-gnu",
    )
    .unwrap();
    let err = verify_artifact_bytes(&verified, "hya-backend", b"tampered")
        .expect_err("tampered artifact must fail");
    assert!(matches!(err, UpdaterError::ArtifactDigestMismatch { .. }));
}

#[test]
fn higher_sequence_recovery_release_may_advance_after_floor() {
    let root = tempdir();
    verify_and_stage(&root, 1, 0, b"v1", "hya-backend");
    journal_prepare(&root, 1, 0).unwrap();
    commit_activation(&root, 1).unwrap();

    // Authorized recovery is just a higher sequence (floor never decreases).
    verify_and_stage(&root, 3, 1, b"recovery-bits", "hya-backend");
    journal_prepare(&root, 3, 1).unwrap();
    let selector = commit_activation(&root, 3).unwrap();
    assert_eq!(selector.current_sequence, 3);
    assert_eq!(selector.accepted_floor, 3);
    // Cannot go back to sequence 2 after floor is 3.
    assert!(commit_activation(&root, 2).is_err());

    std::fs::remove_dir_all(&root).ok();
}
