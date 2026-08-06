use serde::{Deserialize, Serialize};

/// Trusted verifying key for one release-signing identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustRoot {
    /// Stable id matched against release metadata `key_id`.
    pub key_id: String,
    /// Ed25519 verifying key bytes (32).
    pub verifying_key: [u8; 32],
}

/// Monotonic floor of already-accepted release sequences.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AcceptedFloor {
    /// Highest sequence number already accepted; candidates must exceed this.
    pub sequence: u64,
}

/// One artifact covered by signed release metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactDigest {
    /// Relative path of the artifact inside the package tree.
    pub name: String,
    /// Exact byte length of the artifact payload.
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
    /// Monotonic release generation; must advance the accepted floor.
    pub sequence: u64,
    /// Target platform string compared to the host (e.g. `linux-x86_64`).
    pub platform: String,
    /// Artifacts covered by this signed release.
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
    /// Trust-root key id that produced `signature`.
    pub key_id: String,
    /// Ed25519 signature over the domain-separated canonical payload.
    pub signature: Vec<u8>,
}

/// Successfully verified release intent (no candidate code loaded).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedRelease {
    /// Verified monotonic generation number.
    pub sequence: u64,
    /// Verified platform string.
    pub platform: String,
    /// Artifact digests accepted during verification.
    pub artifacts: Vec<ArtifactDigest>,
    /// Whether this release is marked as recovery (see metadata policy).
    pub recovery: bool,
    /// Protocol version that passed verification.
    pub protocol_version: u32,
    /// Minimum updater version required by this release.
    pub min_updater_version: String,
    /// Trust-root key id that signed the metadata.
    pub key_id: String,
}
