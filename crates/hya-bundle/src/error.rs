use thiserror::Error;

/// Typed failure at the source-preparation or prepared-catalog boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BundleError {
    #[error("bundle source I/O failed at `{path}`: {detail}")]
    Io { path: String, detail: String },
    #[error("bundle source `{source_name}` contains duplicate path `{path}`")]
    DuplicateSourcePath { source_name: String, path: String },
    #[error("bundle source `{source_name}` contains invalid path `{path}`")]
    InvalidSourcePath { source_name: String, path: String },
    #[error("bundle source `{source_name}` has no supported v1 manifest")]
    UnsupportedSource { source_name: String },
    #[error("invalid bundle manifest in `{source_name}`: {detail}")]
    InvalidManifest { source_name: String, detail: String },
    #[error("unsupported bundle api version `{found}` in `{source_name}`")]
    WrongApiVersion { source_name: String, found: String },
    #[error("unsupported bundle kind `{found}` in `{source_name}`")]
    WrongKind { source_name: String, found: String },
    #[error("bundle `{bundle_id}` references missing source path `{path}`")]
    MissingReference { bundle_id: String, path: String },
    #[error("bundle `{bundle_id}` contains unsupported executable feature `{feature}`")]
    UnsupportedBundleFeature { bundle_id: String, feature: String },
    #[error("duplicate bundle id `{bundle_id}`")]
    DuplicateBundleId { bundle_id: String },
    #[error("duplicate stable agent id `{stable_id}`")]
    DuplicateStableAgentId { stable_id: String },
    #[error("UNKNOWN_AGENT_ID: `{agent_id}`")]
    UnknownAgentId { agent_id: String },
    #[error("AGENT_SPAWN_NOT_ALLOWED: `{caller}` cannot spawn `{agent_id}`")]
    AgentSpawnNotAllowed { caller: String, agent_id: String },
    #[error("duplicate local agent id `{local_id}` in bundle `{bundle_id}`")]
    DuplicateLocalAgentId { bundle_id: String, local_id: String },
    #[error(
        "agent `{agent_id}` in bundle `{bundle_id}` references unknown spawn target `{reference}`"
    )]
    UnknownAgentReference {
        bundle_id: String,
        agent_id: String,
        reference: String,
    },
    #[error("namespace collision for `{name}` in bundle `{bundle_id}`")]
    NamespaceCollision { bundle_id: String, name: String },
    #[error("alias collision for `{name}` in bundle `{bundle_id}`")]
    AliasCollision { bundle_id: String, name: String },
    #[error("unknown {kind} resource `{reference}` from bundle `{bundle_id}`")]
    UnknownResourceReference {
        bundle_id: String,
        kind: String,
        reference: String,
    },
    #[error("bundle `{bundle_id}` has invalid identity `{value}`")]
    InvalidIdentity { bundle_id: String, value: String },
    #[error("prepared catalog encoding failed: {detail}")]
    PreparedEncode { detail: String },
    #[error("prepared catalog decoding failed: {detail}")]
    PreparedDecode { detail: String },
    #[error("prepared catalog digest mismatch: expected `{expected}`, got `{actual}`")]
    PreparedDigestMismatch { expected: String, actual: String },
    #[error("prepared bundle `{bundle_id}` digest does not cover its canonical content")]
    PreparedBundleDigestMismatch { bundle_id: String },
    #[error("prepared bundle `{bundle_id}` content digest mismatch at `{source_path}`")]
    PreparedContentDigestMismatch {
        bundle_id: String,
        source_path: String,
    },
    #[error("prepared catalog index does not match its canonical bundles")]
    PreparedIndexMismatch,
    #[error("prepared catalog is not canonically ordered")]
    NonCanonicalPreparedCatalog,
    #[error("prepared catalog contains no bundles")]
    EmptyPreparedCatalog,
    #[error("invalid bundle package format")]
    InvalidPackageFormat,
    #[error("PACKAGE_CORRUPT")]
    CorruptPackage,
    #[error("PACKAGE_UNSAFE")]
    UnsafePackage,
    #[error("PACKAGE_LIMIT_EXCEEDED: {limit}")]
    PackageLimitExceeded { limit: &'static str },
    #[error("unsupported bundle package version `{found}`")]
    UnsupportedPackageVersion { found: u16 },
    #[error("private bundle ciphertext digest mismatch")]
    PrivateCiphertextDigestMismatch,
}
