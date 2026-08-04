//! Independent update trust boundary.
//!
//! This crate must not depend on `hya-core`, `hya-plugin`, `hya-mcp`,
//! `hya-bundle`, `hya-app`, or session storage. It verifies signed release
//! metadata and stages/activates immutable runtime generations under a root
//! directory owned by the updater TCB.
//!
//! Production activation remains owner-gated: this library is the verifier and
//! crash journal only. `install.sh` remains break-glass bootstrap/recovery.

mod error;
mod journal;
mod layout;
mod metadata;
mod smoke;
mod stage;
mod verify;

pub use error::UpdaterError;
pub use journal::{
    ActivationJournalRecord, ActivationPhase, ActivationSelector, commit_activation,
    journal_prepare, read_floor, read_selector, recover_activation,
};
pub use layout::{
    UpdaterLayout, assert_no_session_or_secret_reads, assert_tcb_outside_candidate, layout,
    release_directory,
};
pub use metadata::{
    AcceptedFloor, ArtifactDigest, ReleaseMetadata, TrustRoot, VerifiedRelease,
};
pub use smoke::smoke_staged_release;
pub use stage::{StagedRelease, stage_verified_release};
pub use verify::{
    METADATA_DOMAIN, canonical_metadata_payload, verify_artifact_bytes, verify_release_metadata,
};
