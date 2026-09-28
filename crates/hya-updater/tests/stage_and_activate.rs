//! Update staging, smoke, and owner-gated activation.

#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::path::{Path, PathBuf};

fn prepare(root: &std::path::Path, sequence: u64, previous: u64) {
    let owner = UpdaterOwner::acquire(root).unwrap();
    let generation = read_selector(root).unwrap().generation;
    let authorization = owner.authorize(root, sequence, generation).unwrap();
    owner
        .prepare_activation(root, &authorization, previous)
        .unwrap();
}

fn activate(
    root: &std::path::Path,
    sequence: u64,
    previous: u64,
) -> hya_updater::ActivationSelector {
    let owner = UpdaterOwner::acquire(root).unwrap();
    let generation = read_selector(root).unwrap().generation;
    let authorization = owner.authorize(root, sequence, generation).unwrap();
    owner
        .prepare_activation(root, &authorization, previous)
        .unwrap();
    owner.commit_activation(root, &authorization).unwrap()
}

fn commit_existing(
    root: &std::path::Path,
    sequence: u64,
) -> Result<hya_updater::ActivationSelector, UpdaterError> {
    let owner = UpdaterOwner::acquire(root)?;
    let generation = read_selector(root)?.generation;
    let authorization = owner.authorize(root, sequence, generation)?;
    owner.commit_activation(root, &authorization)
}

fn authorization(root: &std::path::Path, sequence: u64) -> hya_updater::ActivationAuthorization {
    let owner = UpdaterOwner::acquire(root).unwrap();
    let generation = read_selector(root).unwrap().generation;
    owner.authorize(root, sequence, generation).unwrap()
}

use ed25519_dalek::{Signer, SigningKey};
use hya_updater::{
    AcceptedFloor, ApplyOptions, ArtifactDigest, ReleaseMetadata, SUPPORTED_PROTOCOL_VERSION,
    TrustRoot, UpdaterError, UpdaterOwner, apply_update, assert_no_session_or_secret_reads,
    assert_tcb_outside_candidate, discard_staged_release, layout, load_trust_roots, read_selector,
    recover_activation, smoke_staged_release, stage_verified_release, verify_artifact_bytes,
    verify_release_metadata, write_trust_roots,
};
use sha2::{Digest, Sha256};

fn tempdir(prefix: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "hya-updater-{prefix}-{}-{}",
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
    let root = TrustRoot {
        key_id: "ci".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    };
    (metadata, root)
}

fn verify_and_stage(
    root: &Path,
    sequence: u64,
    floor: u64,
    bytes: &[u8],
    name: &str,
) -> hya_updater::StagedRelease {
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
    stage_verified_release(root, &verified, &[(name.to_string(), bytes.to_vec())]).unwrap()
}

#[test]
fn stage_then_commit_advances_floor_and_selector() {
    let root = tempdir("stage");
    let bytes = b"artifact-v1";
    let staged = verify_and_stage(&root, 1, 0, bytes, "hya");
    assert!(staged.directory().join("hya").is_file());

    // Immutable: restage same sequence fails.
    let signing = SigningKey::from_bytes(&[3u8; 32]);
    let (metadata, trust) = signed_release(&signing, 1, "hya", bytes);
    let verified = verify_release_metadata(
        &metadata,
        &[trust],
        &AcceptedFloor { sequence: 0 },
        100,
        "x86_64-unknown-linux-gnu",
    )
    .unwrap();
    assert!(
        stage_verified_release(&root, &verified, &[("hya".to_string(), bytes.to_vec())],).is_err()
    );

    let selector = activate(&root, 1, 0);
    assert_eq!(selector.current_sequence, 1);
    assert_eq!(selector.accepted_floor, 1);
    assert_eq!(read_selector(&root).unwrap().current_sequence, 1);

    // Anti-rollback: cannot commit lower/equal sequence after floor advanced.
    assert!(commit_existing(&root, 1).is_err());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn recover_prepare_without_selector_keeps_previous_generation() {
    let root = tempdir("recover-keep");
    verify_and_stage(&root, 1, 0, b"v1", "hya");
    activate(&root, 1, 0);

    verify_and_stage(&root, 2, 1, b"v2", "hya");
    prepare(&root, 2, 1);
    // Crash before selector rename: recover must keep generation 1 and floor 1.
    let recovered = recover_activation(&root).unwrap();
    assert_eq!(recovered.current_sequence, 1);
    assert_eq!(recovered.accepted_floor, 1);
    // Floor never decrements on aborted prepare.
    assert!(commit_existing(&root, 1).is_err());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn recover_prepare_after_selector_switch_finishes_commit() {
    let root = tempdir("recover-finish");
    verify_and_stage(&root, 1, 0, b"v1", "hya");
    activate(&root, 1, 0);

    verify_and_stage(&root, 2, 1, b"v2", "hya");
    prepare(&root, 2, 1);
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
fn recover_ignores_stale_selector_temp_file() {
    let root = tempdir("recover-tmp");
    verify_and_stage(&root, 1, 0, b"v1", "hya");
    activate(&root, 1, 0);
    // Crash left a temp selector behind; recovery must not use it as authority.
    std::fs::write(root.join("current.tmp"), "99\n").unwrap();
    let recovered = recover_activation(&root).unwrap();
    assert_eq!(recovered.current_sequence, 1);
    assert_eq!(recovered.accepted_floor, 1);
    assert!(!root.join("current.tmp").exists() || recovered.current_sequence == 1);

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn smoke_runs_in_dedicated_subprocess() {
    let root = tempdir("smoke");
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
    let root = tempdir("layout");
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
    let (metadata, trust) = signed_release(&signing, 1, "hya", b"good-bytes");
    let verified = verify_release_metadata(
        &metadata,
        &[trust],
        &AcceptedFloor { sequence: 0 },
        100,
        "x86_64-unknown-linux-gnu",
    )
    .unwrap();
    let err = verify_artifact_bytes(&verified, "hya", b"tampered")
        .expect_err("tampered artifact must fail");
    assert!(matches!(err, UpdaterError::ArtifactDigestMismatch { .. }));
}

#[test]
fn higher_sequence_recovery_release_may_advance_after_floor() {
    let root = tempdir("recovery-seq");
    verify_and_stage(&root, 1, 0, b"v1", "hya");
    activate(&root, 1, 0);

    // Authorized recovery is just a higher sequence (floor never decreases).
    verify_and_stage(&root, 3, 1, b"recovery-bits", "hya");
    let selector = activate(&root, 3, 1);
    assert_eq!(selector.current_sequence, 3);
    assert_eq!(selector.accepted_floor, 3);
    // Cannot go back to sequence 2 after floor is 3.
    assert!(commit_existing(&root, 2).is_err());

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn apply_pipeline_stages_without_owner_auth_and_activates_with_flag() {
    let root = tempdir("apply");
    let package = tempdir("package");
    let signing = SigningKey::from_bytes(&[3u8; 32]);
    let bytes = b"payload-v2";
    std::fs::write(package.join("hya"), bytes).unwrap();
    let (metadata, trust) = signed_release(&signing, 2, "hya", bytes);
    write_trust_roots(&layout(&root).trust_roots, &[trust]).unwrap();
    assert_eq!(
        load_trust_roots(&layout(&root).trust_roots).unwrap().len(),
        1
    );

    // Seed floor 1 so sequence 2 is a real advance.
    verify_and_stage(&root, 1, 0, b"v1", "hya");
    activate(&root, 1, 0);

    let staged_only = apply_update(ApplyOptions {
        updater_root: &root,
        metadata: &metadata,
        package_source: package.to_str().unwrap(),
        trust_roots: None,
        host_platform: "x86_64-unknown-linux-gnu",
        now_unix: 100,
        smoke_command: None,
        smoke_args: &[],
        activation: None,
    })
    .unwrap();
    assert!(staged_only.activated.is_none());
    assert_eq!(read_selector(&root).unwrap().current_sequence, 1);
    assert_eq!(read_selector(&root).unwrap().accepted_floor, 1);

    // Second apply of same sequence fails because already staged.
    assert!(
        apply_update(ApplyOptions {
            updater_root: &root,
            metadata: &metadata,
            package_source: package.to_str().unwrap(),
            trust_roots: None,
            host_platform: "x86_64-unknown-linux-gnu",
            now_unix: 100,
            smoke_command: None,
            smoke_args: &[],
            activation: None,
        })
        .is_err()
    );

    // Discard uncommitted candidate and re-apply with explicit owner authorization.
    discard_staged_release(&root, 2).unwrap();
    let authorization = authorization(&root, 2);
    let activated = apply_update(ApplyOptions {
        updater_root: &root,
        metadata: &metadata,
        package_source: package.to_str().unwrap(),
        trust_roots: None,
        host_platform: "x86_64-unknown-linux-gnu",
        now_unix: 100,
        smoke_command: None,
        smoke_args: &[],
        activation: Some(&authorization),
    })
    .unwrap();
    assert_eq!(activated.activated.unwrap().current_sequence, 2);
    assert_eq!(read_selector(&root).unwrap().accepted_floor, 2);

    // Cannot discard the live generation.
    assert!(discard_staged_release(&root, 2).is_err());

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&package).ok();
}

#[test]
fn remote_package_scheme_is_rejected() {
    let err = hya_updater::resolve_package_source("https://example.com/pkg")
        .expect_err("network schemes stay outside TCB");
    assert!(matches!(err, UpdaterError::InvalidMetadata(_)));
}
