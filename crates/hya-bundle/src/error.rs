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
    /// Neither `bundle.yaml` nor `bundle.hya.md` (or both) — no usable manifest.
    #[error("bundle source `{source_name}` has no supported manifest")]
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
    /// A manifest still carries a key the single-agent format removed.
    ///
    /// Reported per key with concrete guidance: `deny_unknown_fields` alone
    /// produces a serde message that does not say what to write instead.
    #[error("`{key}` was removed from the AgentBundle manifest in `{source_name}`: {guidance}")]
    RemovedManifestKey {
        /// Source root or package name for diagnostics.
        source_name: String,
        /// The removed key that is still present.
        key: String,
        /// What the author should do instead.
        guidance: String,
    },
    /// `kind` is not `AgentBundle`.
    #[error("unsupported bundle kind `{found}` in `{source_name}`")]
    WrongKind {
        /// Source root or package name for diagnostics.
        source_name: String,
        /// Value found in the manifest.
        found: String,
    },
    /// Workflow source could not be compiled by the shared Workflow compiler.
    #[error("Workflow source `{source_path}` in bundle `{bundle_id}` failed to compile: {detail}")]
    WorkflowCompile {
        /// Bundle identity owning the Workflow source.
        bundle_id: String,
        /// Canonical Workflow source path.
        source_path: String,
        /// Compiler diagnostic detail.
        detail: String,
    },
    /// Manifest Workflow identity differs from the compiled Workflow name.
    #[error(
        "WorkflowBundle `{bundle_id}` declares Workflow `{manifest_id}`, but the source compiles as `{compiled_id}`"
    )]
    WorkflowIdMismatch {
        /// Bundle identity owning the Workflow.
        bundle_id: String,
        /// Identifier declared in `bundle.yaml`.
        manifest_id: String,
        /// Identifier produced by `hya-workflow::compile`.
        compiled_id: String,
    },
    /// A Workflow stage or reachable Agent names a non-built-in Agent absent from the bundle.
    #[error(
        "WorkflowBundle `{bundle_id}` is missing reachable Agent `{agent_id}` referenced by `{reference}`"
    )]
    WorkflowAgentMissing {
        /// Bundle identity owning the Workflow.
        bundle_id: String,
        /// Missing stable Agent id.
        agent_id: String,
        /// Stage, verifier, or Agent that required the missing id.
        reference: String,
    },
    /// A WorkflowBundle carries an Agent outside the exact compiled reachable closure.
    #[error("WorkflowBundle `{bundle_id}` carries unreachable Agent `{agent_id}`")]
    WorkflowAgentUnreachable {
        /// Bundle identity owning the Workflow.
        bundle_id: String,
        /// Unreachable stable Agent id.
        agent_id: String,
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
    /// A `resource_view` entry names a resource outside the agent's tool plane.
    ///
    /// Distinct from [`Self::UnknownResourceReference`] on purpose: the resource
    /// may well exist, but this agent's plane does not admit it. Saying
    /// "unknown" would send an author looking for a typo that is not there.
    #[error(
        "bundle `{bundle_id}` references `{reference}`, which is outside its `{plane}` tool plane"
    )]
    ResourceNotInPlane {
        /// Bundle identity id of the referring agent.
        bundle_id: String,
        /// The rejected reference.
        reference: String,
        /// Plane the referring agent is bound to.
        plane: String,
    },
    /// An installed bundle claims an agent id reserved by a built-in agent.
    ///
    /// Built-ins run on the full Harness plane; an installed bundle agent runs
    /// on the clamped internal-public plane. Letting a bundle claim a built-in
    /// id would make the plane of a well-known id depend on install order.
    #[error(
        "bundle `{bundle_id}` declares agent `{agent_id}`, which is a reserved built-in agent id"
    )]
    BuiltinAgentIdShadowed {
        /// Bundle identity id.
        bundle_id: String,
        /// Conflicting agent id.
        agent_id: String,
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
