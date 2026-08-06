use thiserror::Error;

/// Fail-closed updater verification and activation errors.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum UpdaterError {
    /// Metadata `key_id` is not present in the configured trust roots.
    #[error("unknown trust root key id `{key_id}`")]
    UnknownTrustRoot {
        /// Key id from the signed metadata that failed lookup.
        key_id: String,
    },
    /// Trust-root verifying-key bytes are not a valid Ed25519 public key.
    #[error("invalid trust root public key")]
    InvalidTrustRootKey,
    /// Ed25519 signature over the canonical metadata payload failed.
    #[error("invalid release signature")]
    InvalidSignature,
    /// Candidate sequence does not strictly advance the accepted anti-rollback floor.
    #[error("release sequence {sequence} does not advance accepted floor {floor}")]
    NonIncreasingSequence {
        /// Sequence claimed by the candidate release.
        sequence: u64,
        /// Current accepted floor that must be exceeded.
        floor: u64,
    },
    /// Signed platform string does not match the host platform string.
    #[error("platform mismatch: metadata `{got}` does not match host `{want}`")]
    PlatformMismatch {
        /// Platform from release metadata.
        got: String,
        /// Host platform expected by the caller.
        want: String,
    },
    /// Current time is before metadata `not_before`.
    #[error("release metadata is not yet valid")]
    NotYetValid,
    /// Current time is after metadata `not_after` (or freeze policy rejected it).
    #[error("release metadata expired or frozen")]
    Expired,
    /// Metadata `protocol_version` is not supported by this crate.
    #[error("unsupported metadata protocol version {got} (supported {supported})")]
    UnsupportedProtocol {
        /// Protocol version from metadata.
        got: u32,
        /// Highest protocol this crate understands.
        supported: u32,
    },
    /// Running updater package version is older than metadata `min_updater_version`.
    #[error("updater version {have} is below required min_updater_version {need}")]
    UpdaterTooOld {
        /// Version of this updater crate/package.
        have: String,
        /// Minimum required by the signed metadata.
        need: String,
    },
    /// SHA-256 of a named artifact does not match the signed digest.
    #[error("artifact digest mismatch for `{name}`")]
    ArtifactDigestMismatch {
        /// Artifact path/name under the package directory.
        name: String,
    },
    /// Staged smoke command exited non-zero or could not run.
    #[error("staged release smoke failed: {0}")]
    SmokeFailed(String),
    /// Candidate layout would place or replace TCB control files incorrectly.
    #[error("updater ownership layout violation: {0}")]
    OwnershipViolation(String),
    /// Activation was requested without `owner_authorized` / CLI owner gate.
    #[error("activation requires owner authorization")]
    ActivationNotAuthorized,
    /// Malformed metadata, bad paths, I/O, or other non-crypto validation failure.
    #[error("invalid release metadata: {0}")]
    InvalidMetadata(String),
}
