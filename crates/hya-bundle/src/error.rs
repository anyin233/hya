//! Typed failures at the AgentBundle prepare, package, and catalog boundaries.

use thiserror::Error;

/// Typed failure at the source-preparation or prepared-catalog boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BundleError {
    /// Filesystem read/write failed while loading or staging a source/package.
    #[error("bundle source I/O failed at `{path}`: {detail}")]
    Io {
        /// Path that failed (display form).
        path: String,
        /// Underlying OS/error detail.
        detail: String,
    },
    /// Two files in one source tree map to the same logical relative path.
    #[error("bundle source `{source_name}` contains duplicate path `{path}`")]
    DuplicateSourcePath {
        /// Source root or package name for diagnostics.
        source_name: String,
        /// Conflicting relative path.
        path: String,
    },
    /// Path is unsafe (symlink, traversal, non-UTF8 component, etc.).
    #[error("bundle source `{source_name}` contains invalid path `{path}`")]
    InvalidSourcePath {
        /// Source root or package name for diagnostics.
        source_name: String,
        /// Offending path.
        path: String,
    },
    /// Neither `bundle.yaml` nor `bundle.hya.md` (or both) — no usable v1 manifest.
    #[error("bundle source `{source_name}` has no supported v1 manifest")]
    UnsupportedSource {
        /// Source root or package name for diagnostics.
        source_name: String,
    },
    /// Manifest YAML failed parse or `deny_unknown_fields` validation.
    #[error("invalid bundle manifest in `{source_name}`: {detail}")]
    InvalidManifest {
        /// Source root or package name for diagnostics.
        source_name: String,
        /// Parse/validation detail.
        detail: String,
    },
    /// `api_version` is not `hya.agent-bundle/v1`.
    #[error("unsupported bundle api version `{found}` in `{source_name}`")]
    WrongApiVersion {
        /// Source root or package name for diagnostics.
        source_name: String,
        /// Value found in the manifest.
        found: String,
    },
    /// `kind` is not `AgentBundle`.
    #[error("unsupported bundle kind `{found}` in `{source_name}`")]
    WrongKind {
        /// Source root or package name for diagnostics.
        source_name: String,
        /// Value found in the manifest.
        found: String,
    },
    /// Declared prompt/resource path is missing from the source closure.
    #[error("bundle `{bundle_id}` references missing source path `{path}`")]
    MissingReference {
        /// Bundle identity id.
        bundle_id: String,
        /// Missing relative path.
        path: String,
    },
    /// Feature declared in source is not supported by this prepare/runtime path.
    #[error("bundle `{bundle_id}` contains unsupported executable feature `{feature}`")]
    UnsupportedBundleFeature {
        /// Bundle identity id.
        bundle_id: String,
        /// Feature key (for example `resources.mcp`).
        feature: String,
    },
    /// Two prepared bundles share the same `identity.id`.
    #[error("duplicate bundle id `{bundle_id}`")]
    DuplicateBundleId {
        /// Conflicting bundle id.
        bundle_id: String,
    },
    /// Two agents across the catalog share the same global `stable_id`.
    #[error("duplicate stable agent id `{stable_id}`")]
    DuplicateStableAgentId {
        /// Conflicting stable agent id.
        stable_id: String,
    },
    /// Lookup/spawn requested an agent id that is not in the catalog.
    #[error("UNKNOWN_AGENT_ID: `{agent_id}`")]
    UnknownAgentId {
        /// Requested agent id (stable or qualified).
        agent_id: String,
    },
    /// Caller is not allowed to spawn the requested agent (`can_spawn` graph).
    #[error("AGENT_SPAWN_NOT_ALLOWED: `{caller}` cannot spawn `{agent_id}`")]
    AgentSpawnNotAllowed {
        /// Caller's stable agent id.
        caller: String,
        /// Requested spawn target stable id.
        agent_id: String,
    },
    /// Two agents in one bundle share the same `local_id`.
    #[error("duplicate local agent id `{local_id}` in bundle `{bundle_id}`")]
    DuplicateLocalAgentId {
        /// Bundle identity id.
        bundle_id: String,
        /// Conflicting local agent id.
        local_id: String,
    },
    /// `can_spawn` / hook / resource reference does not resolve in-catalog.
    #[error(
        "agent `{agent_id}` in bundle `{bundle_id}` references unknown spawn target `{reference}`"
    )]
    UnknownAgentReference {
        /// Bundle identity id.
        bundle_id: String,
        /// Agent that owns the bad reference.
        agent_id: String,
        /// Unresolved reference string.
        reference: String,
    },
    /// Global qualified name already claimed by another export/agent.
    #[error("namespace collision for `{name}` in bundle `{bundle_id}`")]
    NamespaceCollision {
        /// Bundle identity id.
        bundle_id: String,
        /// Colliding qualified or stable name.
        name: String,
    },
    /// Local id or alias collides within a bundle resource set.
    #[error("alias collision for `{name}` in bundle `{bundle_id}`")]
    AliasCollision {
        /// Bundle identity id.
        bundle_id: String,
        /// Colliding local name or alias.
        name: String,
    },
    /// Resource reference (tool/skill/hook/extension) cannot be resolved.
    #[error("unknown {kind} resource `{reference}` from bundle `{bundle_id}`")]
    UnknownResourceReference {
        /// Bundle identity id used for local resolution.
        bundle_id: String,
        /// Resource kind label (`tool`, `skill`, `hook`, …).
        kind: String,
        /// Unresolved local or qualified reference.
        reference: String,
    },
    /// Bundle id/version/publisher failed identity validation rules.
    #[error("bundle `{bundle_id}` has invalid identity `{value}`")]
    InvalidIdentity {
        /// Bundle identity id (or attempted id).
        bundle_id: String,
        /// Invalid field value.
        value: String,
    },
    /// Serializing the prepared catalog document failed.
    #[error("prepared catalog encoding failed: {detail}")]
    PreparedEncode {
        /// Serializer detail.
        detail: String,
    },
    /// Deserializing prepared catalog bytes failed.
    #[error("prepared catalog decoding failed: {detail}")]
    PreparedDecode {
        /// Deserializer detail.
        detail: String,
    },
    /// Caller-supplied catalog digest does not match SHA-256 of the bytes.
    #[error("prepared catalog digest mismatch: expected `{expected}`, got `{actual}`")]
    PreparedDigestMismatch {
        /// Digest the caller expected.
        expected: String,
        /// Digest of the provided bytes.
        actual: String,
    },
    /// Bundle's embedded digest does not match re-hashed canonical content.
    #[error("prepared bundle `{bundle_id}` digest does not cover its canonical content")]
    PreparedBundleDigestMismatch {
        /// Bundle identity id.
        bundle_id: String,
    },
    /// A single prepared resource's content digest does not match its bytes.
    #[error("prepared bundle `{bundle_id}` content digest mismatch at `{source_path}`")]
    PreparedContentDigestMismatch {
        /// Bundle identity id.
        bundle_id: String,
        /// Source path of the mismatched resource.
        source_path: String,
    },
    /// Index rows do not match the prepared bundles vector (ids/versions/digests).
    #[error("prepared catalog index does not match its canonical bundles")]
    PreparedIndexMismatch,
    /// Bundles/index not in the required deterministic order.
    #[error("prepared catalog is not canonically ordered")]
    NonCanonicalPreparedCatalog,
    /// Catalog contains zero bundles after prepare/merge.
    #[error("prepared catalog contains no bundles")]
    EmptyPreparedCatalog,
    /// Bytes do not start with a known public or private package magic.
    #[error("invalid bundle package format")]
    InvalidPackageFormat,
    /// Archive is truncated, invalid, or fails structural checks mid-read.
    #[error("PACKAGE_CORRUPT")]
    CorruptPackage,
    /// Package contains unsafe entries (symlink, device, traversal, non-regular file).
    #[error("PACKAGE_UNSAFE")]
    UnsafePackage,
    /// Archive size, expansion ratio, path length, or depth limit exceeded.
    #[error("PACKAGE_LIMIT_EXCEEDED: {limit}")]
    PackageLimitExceeded {
        /// Which limit fired (for example `archive bytes`).
        limit: &'static str,
    },
    /// Private package version field is not the supported version.
    #[error("unsupported bundle package version `{found}`")]
    UnsupportedPackageVersion {
        /// Version number found in the private header.
        found: u16,
    },
    /// Private package ciphertext SHA-256 does not match the header digest.
    #[error("private bundle ciphertext digest mismatch")]
    PrivateCiphertextDigestMismatch,
}
