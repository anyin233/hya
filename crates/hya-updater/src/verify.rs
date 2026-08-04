use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::UpdaterError;
use crate::metadata::{AcceptedFloor, ArtifactDigest, ReleaseMetadata, TrustRoot, VerifiedRelease};

/// Domain separation for release-metadata signatures.
pub const METADATA_DOMAIN: &[u8] = b"hya.updater.release-metadata.v1";

#[derive(Serialize)]
struct CanonicalMetadata<'a> {
    sequence: u64,
    platform: &'a str,
    artifacts: &'a [ArtifactDigest],
    not_before: i64,
    not_after: i64,
    recovery: bool,
    key_id: &'a str,
}

/// Canonical domain-separated payload used for signing and verification.
///
/// The signature field is intentionally excluded.
pub fn canonical_metadata_payload(
    metadata: &ReleaseMetadata,
) -> Result<Vec<u8>, UpdaterError> {
    if metadata.platform.is_empty() {
        return Err(UpdaterError::InvalidMetadata(
            "platform must be non-empty".to_string(),
        ));
    }
    if metadata.key_id.is_empty() {
        return Err(UpdaterError::InvalidMetadata(
            "key_id must be non-empty".to_string(),
        ));
    }
    if metadata.not_after < metadata.not_before {
        return Err(UpdaterError::InvalidMetadata(
            "not_after must be >= not_before".to_string(),
        ));
    }
    if metadata.artifacts.is_empty() {
        return Err(UpdaterError::InvalidMetadata(
            "artifacts must be non-empty".to_string(),
        ));
    }
    for artifact in &metadata.artifacts {
        if artifact.name.is_empty() {
            return Err(UpdaterError::InvalidMetadata(
                "artifact name must be non-empty".to_string(),
            ));
        }
        if artifact.sha256_hex.len() != 64
            || !artifact
                .sha256_hex
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return Err(UpdaterError::InvalidMetadata(format!(
                "artifact `{}` digest must be 64 lower-hex chars",
                artifact.name
            )));
        }
        // Normalize to lowercase in the canonical form by rejecting upper-case.
        if artifact.sha256_hex.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(UpdaterError::InvalidMetadata(format!(
                "artifact `{}` digest must be lower-hex",
                artifact.name
            )));
        }
    }

    let body = CanonicalMetadata {
        sequence: metadata.sequence,
        platform: &metadata.platform,
        artifacts: &metadata.artifacts,
        not_before: metadata.not_before,
        not_after: metadata.not_after,
        recovery: metadata.recovery,
        key_id: &metadata.key_id,
    };
    let json = serde_json::to_vec(&body).map_err(|error| {
        UpdaterError::InvalidMetadata(format!("serialize canonical metadata: {error}"))
    })?;

    let mut out = Vec::with_capacity(METADATA_DOMAIN.len() + 1 + json.len());
    out.extend_from_slice(METADATA_DOMAIN);
    out.push(0);
    out.extend_from_slice(&json);
    Ok(out)
}

/// Verify signed release metadata against trust roots and anti-rollback floor.
///
/// Does not load candidate runtime code, session databases, or extension
/// authorities.
pub fn verify_release_metadata(
    metadata: &ReleaseMetadata,
    roots: &[TrustRoot],
    floor: &AcceptedFloor,
    now_unix: i64,
    host_platform: &str,
) -> Result<VerifiedRelease, UpdaterError> {
    if metadata.sequence <= floor.sequence {
        return Err(UpdaterError::NonIncreasingSequence {
            sequence: metadata.sequence,
            floor: floor.sequence,
        });
    }
    if metadata.platform != host_platform {
        return Err(UpdaterError::PlatformMismatch {
            got: metadata.platform.clone(),
            want: host_platform.to_string(),
        });
    }
    if now_unix < metadata.not_before {
        return Err(UpdaterError::NotYetValid);
    }
    if now_unix > metadata.not_after {
        return Err(UpdaterError::Expired);
    }

    let root = roots
        .iter()
        .find(|root| root.key_id == metadata.key_id)
        .ok_or_else(|| UpdaterError::UnknownTrustRoot {
            key_id: metadata.key_id.clone(),
        })?;
    let verifying_key = VerifyingKey::from_bytes(&root.verifying_key)
        .map_err(|_| UpdaterError::InvalidTrustRootKey)?;

    let payload = canonical_metadata_payload(metadata)?;
    let signature = Signature::from_slice(&metadata.signature)
        .map_err(|_| UpdaterError::InvalidSignature)?;
    verifying_key
        .verify(&payload, &signature)
        .map_err(|_| UpdaterError::InvalidSignature)?;

    Ok(VerifiedRelease {
        sequence: metadata.sequence,
        platform: metadata.platform.clone(),
        artifacts: metadata.artifacts.clone(),
        recovery: metadata.recovery,
        key_id: metadata.key_id.clone(),
    })
}

/// Verify one on-disk artifact against a verified release digest entry.
pub fn verify_artifact_bytes(
    verified: &VerifiedRelease,
    name: &str,
    bytes: &[u8],
) -> Result<(), UpdaterError> {
    let Some(expected) = verified.artifacts.iter().find(|a| a.name == name) else {
        return Err(UpdaterError::InvalidMetadata(format!(
            "artifact `{name}` not present in verified release"
        )));
    };
    if expected.size != bytes.len() as u64 {
        return Err(UpdaterError::ArtifactDigestMismatch {
            name: name.to_string(),
        });
    }
    let digest = Sha256::digest(bytes);
    let got = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if got != expected.sha256_hex {
        return Err(UpdaterError::ArtifactDigestMismatch {
            name: name.to_string(),
        });
    }
    Ok(())
}
