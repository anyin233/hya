//! Independent update trust boundary.
//!
//! This crate must not depend on `hya-core`, `hya-plugin`, `hya-mcp`,
//! `hya-bundle`, `hya-app`, or session storage. It verifies signed release
//! metadata and stages/activates immutable runtime generations under a root
//! directory owned by the updater TCB.
//!
//! Production activation remains owner-gated: signatures alone never activate.
//! Callers must pass `owner_authorized = true` (CLI: `--owner-authorized-activation`).
//! `install.sh` remains break-glass bootstrap/recovery. Network download is
//! outside this TCB; operators copy a complete local package directory in.

mod error;
mod fetch;
mod journal;
mod layout;
mod metadata;
mod pipeline;
mod smoke;
mod stage;
mod trust;
mod verify;

pub use error::UpdaterError;
pub use fetch::{FetchedArtifact, fetch_artifacts_from_dir, resolve_package_source};
pub use journal::{
    ActivationJournalRecord, ActivationPhase, ActivationSelector, commit_activation,
    journal_prepare, read_floor, read_selector, recover_activation,
};
pub use layout::{
    UpdaterLayout, assert_no_session_or_secret_reads, assert_tcb_outside_candidate, layout,
    release_directory,
};
pub use metadata::{
    AcceptedFloor, ArtifactDigest, ReleaseMetadata, SUPPORTED_PROTOCOL_VERSION, TrustRoot,
    VerifiedRelease,
};
pub use pipeline::{ApplyOptions, ApplyResult, apply_update, discard_staged_release};
pub use smoke::smoke_staged_release;
pub use stage::{StagedRelease, stage_verified_release};
pub use trust::{load_trust_roots, write_trust_roots};
pub use verify::{
    METADATA_DOMAIN, UPDATER_PACKAGE_VERSION, canonical_metadata_payload, verify_artifact_bytes,
    verify_release_metadata,
};
