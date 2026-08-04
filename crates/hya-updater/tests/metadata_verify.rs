#![allow(clippy::unwrap_used, clippy::expect_used)]
use ed25519_dalek::{Signer, SigningKey};
use hya_updater::{
    AcceptedFloor, ArtifactDigest, ReleaseMetadata, TrustRoot, UpdaterError,
    verify_release_metadata,
};
use sha2::{Digest, Sha256};

fn sign_metadata(signing: &SigningKey, metadata: &mut ReleaseMetadata) {
    let payload = hya_updater::canonical_metadata_payload(metadata)
        .expect("canonical payload");
    let sig = signing.sign(&payload);
    metadata.signature = sig.to_bytes().to_vec();
}

fn sample_artifact() -> ArtifactDigest {
    let bytes = b"hya-backend-bytes";
    let digest = Sha256::digest(bytes);
    ArtifactDigest {
        name: "hya-backend".to_string(),
        size: bytes.len() as u64,
        sha256_hex: hex_lower(&digest),
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn valid_signed_metadata_verifies() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let verifying = signing.verifying_key();
    let roots = [TrustRoot {
        key_id: "ci-root-1".to_string(),
        verifying_key: verifying.to_bytes(),
    }];
    let mut metadata = ReleaseMetadata {
        sequence: 3,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        artifacts: vec![sample_artifact()],
        not_before: 1_700_000_000,
        not_after: 2_000_000_000,
        recovery: false,
        key_id: "ci-root-1".to_string(),
        signature: Vec::new(),
    };
    sign_metadata(&signing, &mut metadata);

    let verified = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 2 },
        1_800_000_000,
        "x86_64-unknown-linux-gnu",
    )
    .expect("valid metadata must verify");
    assert_eq!(verified.sequence, 3);
    assert_eq!(verified.platform, "x86_64-unknown-linux-gnu");
    assert!(!verified.recovery);
}

#[test]
fn wrong_signature_is_rejected() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let other = SigningKey::from_bytes(&[9u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci-root-1".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let mut metadata = ReleaseMetadata {
        sequence: 3,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        artifacts: vec![sample_artifact()],
        not_before: 1_700_000_000,
        not_after: 2_000_000_000,
        recovery: false,
        key_id: "ci-root-1".to_string(),
        signature: Vec::new(),
    };
    // Sign with the wrong key.
    let payload = hya_updater::canonical_metadata_payload(&metadata).unwrap();
    metadata.signature = other.sign(&payload).to_bytes().to_vec();

    let err = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 2 },
        1_800_000_000,
        "x86_64-unknown-linux-gnu",
    )
    .expect_err("wrong signature must fail closed");
    assert_eq!(err, UpdaterError::InvalidSignature);
}

#[test]
fn non_increasing_sequence_is_rejected() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci-root-1".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let mut metadata = ReleaseMetadata {
        sequence: 2,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        artifacts: vec![sample_artifact()],
        not_before: 1_700_000_000,
        not_after: 2_000_000_000,
        recovery: false,
        key_id: "ci-root-1".to_string(),
        signature: Vec::new(),
    };
    sign_metadata(&signing, &mut metadata);

    let err = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 2 },
        1_800_000_000,
        "x86_64-unknown-linux-gnu",
    )
    .expect_err("sequence must strictly advance floor");
    assert_eq!(
        err,
        UpdaterError::NonIncreasingSequence {
            sequence: 2,
            floor: 2
        }
    );
}

#[test]
fn platform_mismatch_is_rejected() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci-root-1".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let mut metadata = ReleaseMetadata {
        sequence: 5,
        platform: "aarch64-apple-darwin".to_string(),
        artifacts: vec![sample_artifact()],
        not_before: 1_700_000_000,
        not_after: 2_000_000_000,
        recovery: false,
        key_id: "ci-root-1".to_string(),
        signature: Vec::new(),
    };
    sign_metadata(&signing, &mut metadata);

    let err = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 1 },
        1_800_000_000,
        "x86_64-unknown-linux-gnu",
    )
    .expect_err("platform must match host");
    assert!(matches!(err, UpdaterError::PlatformMismatch { .. }));
}

#[test]
fn expired_metadata_is_rejected() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci-root-1".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let mut metadata = ReleaseMetadata {
        sequence: 5,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        artifacts: vec![sample_artifact()],
        not_before: 1_700_000_000,
        not_after: 1_700_000_100,
        recovery: false,
        key_id: "ci-root-1".to_string(),
        signature: Vec::new(),
    };
    sign_metadata(&signing, &mut metadata);

    let err = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 1 },
        1_800_000_000,
        "x86_64-unknown-linux-gnu",
    )
    .expect_err("expired metadata must fail");
    assert_eq!(err, UpdaterError::Expired);
}

#[test]
fn unknown_trust_root_is_rejected() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let roots = [TrustRoot {
        key_id: "other-root".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let mut metadata = ReleaseMetadata {
        sequence: 5,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        artifacts: vec![sample_artifact()],
        not_before: 1_700_000_000,
        not_after: 2_000_000_000,
        recovery: false,
        key_id: "ci-root-1".to_string(),
        signature: Vec::new(),
    };
    sign_metadata(&signing, &mut metadata);

    let err = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 1 },
        1_800_000_000,
        "x86_64-unknown-linux-gnu",
    )
    .expect_err("unknown key id must fail closed");
    assert_eq!(
        err,
        UpdaterError::UnknownTrustRoot {
            key_id: "ci-root-1".to_string()
        }
    );
}

#[test]
fn not_yet_valid_metadata_is_rejected() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci-root-1".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let mut metadata = ReleaseMetadata {
        sequence: 5,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        artifacts: vec![sample_artifact()],
        not_before: 1_900_000_000,
        not_after: 2_000_000_000,
        recovery: false,
        key_id: "ci-root-1".to_string(),
        signature: Vec::new(),
    };
    sign_metadata(&signing, &mut metadata);

    let err = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 1 },
        1_800_000_000,
        "x86_64-unknown-linux-gnu",
    )
    .expect_err("not-yet-valid metadata must fail");
    assert_eq!(err, UpdaterError::NotYetValid);
}
