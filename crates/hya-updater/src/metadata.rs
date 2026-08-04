use serde::{Deserialize, Serialize};

/// Trusted verifying key for one release-signing identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustRoot {
    pub key_id: String,
    /// Ed25519 verifying key bytes (32).
    pub verifying_key: [u8; 32],
}

/// Monotonic floor of already-accepted release sequences.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AcceptedFloor {
    pub sequence: u64,
}

/// One artifact covered by signed release metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactDigest {
    pub name: String,
    pub size: u64,
    /// Lower-hex SHA-256 of the artifact bytes.
    pub sha256_hex: String,
}

/// Current signed-metadata protocol supported by this crate.
pub const SUPPORTED_PROTOCOL_VERSION: u32 = 1;

/// Signed release metadata. The `signature` field is excluded from the
/// canonical payload that is verified.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseMetadata {
    pub sequence: u64,
    pub platform: String,
    pub artifacts: Vec<ArtifactDigest>,
    /// Unix seconds; inclusive.
    pub not_before: i64,
    /// Unix seconds; inclusive.
    pub not_after: i64,
    /// When true, may reinstall a previously accepted generation only if the
    /// sequence still advances the accepted floor (authorized recovery).
    pub recovery: bool,
    /// Metadata protocol version. Must equal [`SUPPORTED_PROTOCOL_VERSION`].
    pub protocol_version: u32,
    /// Minimum updater package version required to apply this release
    /// (semver string compared as `CARGO_PKG_VERSION`).
    pub min_updater_version: String,
    pub key_id: String,
    /// Ed25519 signature over the domain-separated canonical payload.
    pub signature: Vec<u8>,
}

/// Successfully verified release intent (no candidate code loaded).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedRelease {
    pub sequence: u64,
    pub platform: String,
    pub artifacts: Vec<ArtifactDigest>,
    pub recovery: bool,
    pub protocol_version: u32,
    pub min_updater_version: String,
    pub key_id: String,
}
