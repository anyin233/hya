use thiserror::Error;

/// Fail-closed updater verification and activation errors.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum UpdaterError {
    #[error("unknown trust root key id `{key_id}`")]
    UnknownTrustRoot { key_id: String },
    #[error("invalid trust root public key")]
    InvalidTrustRootKey,
    #[error("invalid release signature")]
    InvalidSignature,
    #[error("release sequence {sequence} does not advance accepted floor {floor}")]
    NonIncreasingSequence { sequence: u64, floor: u64 },
    #[error("platform mismatch: metadata `{got}` does not match host `{want}`")]
    PlatformMismatch { got: String, want: String },
    #[error("release metadata is not yet valid")]
    NotYetValid,
    #[error("release metadata expired or frozen")]
    Expired,
    #[error("unsupported metadata protocol version {got} (supported {supported})")]
    UnsupportedProtocol { got: u32, supported: u32 },
    #[error("updater version {have} is below required min_updater_version {need}")]
    UpdaterTooOld { have: String, need: String },
    #[error("artifact digest mismatch for `{name}`")]
    ArtifactDigestMismatch { name: String },
    #[error("staged release smoke failed: {0}")]
    SmokeFailed(String),
    #[error("updater ownership layout violation: {0}")]
    OwnershipViolation(String),
    #[error("activation requires owner authorization")]
    ActivationNotAuthorized,
    #[error("invalid release metadata: {0}")]
    InvalidMetadata(String),
}
