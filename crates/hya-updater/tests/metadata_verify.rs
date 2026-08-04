#![allow(clippy::unwrap_used, clippy::expect_used)]
use ed25519_dalek::{Signer, SigningKey};
use hya_updater::{
    AcceptedFloor, ArtifactDigest, ReleaseMetadata, SUPPORTED_PROTOCOL_VERSION, TrustRoot,
    UPDATER_PACKAGE_VERSION, UpdaterError, verify_release_metadata,
};
use sha2::{Digest, Sha256};

fn sign_metadata(signing: &SigningKey, metadata: &mut ReleaseMetadata) {
    let payload = hya_updater::canonical_metadata_payload(metadata).expect("canonical payload");
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

fn base_metadata(sequence: u64, key_id: &str) -> ReleaseMetadata {
    ReleaseMetadata {
        sequence,
        platform: "x86_64-unknown-linux-gnu".to_string(),
        artifacts: vec![sample_artifact()],
        not_before: 1_700_000_000,
        not_after: 2_000_000_000,
        recovery: false,
        protocol_version: SUPPORTED_PROTOCOL_VERSION,
        min_updater_version: "0.34.0".to_string(),
        key_id: key_id.to_string(),
        signature: Vec::new(),
    }
}

#[test]
fn valid_signed_metadata_verifies() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let verifying = signing.verifying_key();
    let roots = [TrustRoot {
        key_id: "ci-root-1".to_string(),
        verifying_key: verifying.to_bytes(),
    }];
    let mut metadata = base_metadata(3, "ci-root-1");
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
    assert_eq!(verified.protocol_version, SUPPORTED_PROTOCOL_VERSION);
}

#[test]
fn wrong_signature_is_rejected() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let other = SigningKey::from_bytes(&[9u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci-root-1".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let mut metadata = base_metadata(3, "ci-root-1");
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
    let mut metadata = base_metadata(2, "ci-root-1");
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
    let mut metadata = base_metadata(5, "ci-root-1");
    metadata.platform = "aarch64-apple-darwin".to_string();
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
    let mut metadata = base_metadata(5, "ci-root-1");
    metadata.not_after = 1_700_000_100;
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
    let mut metadata = base_metadata(5, "ci-root-1");
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
    let mut metadata = base_metadata(5, "ci-root-1");
    metadata.not_before = 1_900_000_000;
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

#[test]
fn unsupported_protocol_is_rejected() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci-root-1".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let mut metadata = base_metadata(5, "ci-root-1");
    metadata.protocol_version = SUPPORTED_PROTOCOL_VERSION + 1;
    sign_metadata(&signing, &mut metadata);

    let err = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 1 },
        1_800_000_000,
        "x86_64-unknown-linux-gnu",
    )
    .expect_err("future protocol must fail closed");
    assert!(matches!(err, UpdaterError::UnsupportedProtocol { .. }));
}

#[test]
fn min_updater_version_too_new_is_rejected() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci-root-1".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let mut metadata = base_metadata(5, "ci-root-1");
    // Far-future requirement relative to this package version.
    metadata.min_updater_version = "99.0.0".to_string();
    sign_metadata(&signing, &mut metadata);

    let err = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 1 },
        1_800_000_000,
        "x86_64-unknown-linux-gnu",
    )
    .expect_err("too-old updater must fail");
    assert_eq!(
        err,
        UpdaterError::UpdaterTooOld {
            have: UPDATER_PACKAGE_VERSION.to_string(),
            need: "99.0.0".to_string(),
        }
    );
}

#[test]
fn replaying_already_accepted_sequence_is_rejected() {
    let signing = SigningKey::from_bytes(&[7u8; 32]);
    let roots = [TrustRoot {
        key_id: "ci-root-1".to_string(),
        verifying_key: signing.verifying_key().to_bytes(),
    }];
    let mut metadata = base_metadata(4, "ci-root-1");
    sign_metadata(&signing, &mut metadata);

    // Floor already at 4 after a prior commit — classic anti-replay.
    let err = verify_release_metadata(
        &metadata,
        &roots,
        &AcceptedFloor { sequence: 4 },
        1_800_000_000,
        "x86_64-unknown-linux-gnu",
    )
    .expect_err("replay at accepted floor must fail");
    assert_eq!(
        err,
        UpdaterError::NonIncreasingSequence {
            sequence: 4,
            floor: 4
        }
    );
}
