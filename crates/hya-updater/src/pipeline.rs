//! Opt-in apply pipeline: recover → verify → fetch → stage → smoke → (optional) activate.
//!
//! Activation never runs unless `owner_authorized` is true. That flag is the
//! product-side stand-in for the plan's owner production-activation gate; it is
//! not granted by a valid signature alone.

use std::fs;
use std::path::Path;

use crate::error::UpdaterError;
use crate::fetch::{fetch_artifacts_from_dir, resolve_package_source};
use crate::journal::{
    ActivationSelector, commit_activation, journal_prepare, read_selector, recover_activation,
};
use crate::layout::{assert_no_session_or_secret_reads, assert_tcb_outside_candidate, layout};
use crate::metadata::{AcceptedFloor, ReleaseMetadata, TrustRoot, VerifiedRelease};
use crate::smoke::smoke_staged_release;
use crate::stage::{StagedRelease, stage_verified_release};
use crate::trust::load_trust_roots;
use crate::verify::verify_release_metadata;

/// Result of a staged (and optionally activated) apply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplyResult {
    /// Release intent that passed signature and policy checks.
    pub verified: VerifiedRelease,
    /// Immutable staged generation under `releases/<sequence>/`.
    pub staged: StagedRelease,
    /// New selector after owner-authorized activation; `None` if only staged.
    pub activated: Option<ActivationSelector>,
    /// Selector state after crash recovery, before this apply mutated anything.
    pub recovered_before: ActivationSelector,
}

/// Apply options for the independent updater pipeline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplyOptions<'a> {
    /// Updater TCB root that owns trust roots, floor, journal, and releases.
    pub updater_root: &'a Path,
    /// Signed release metadata to verify and apply.
    pub metadata: &'a ReleaseMetadata,
    /// Local package directory or `file://` path holding artifacts.
    pub package_source: &'a str,
    /// Explicit trust roots; when `None`, load `updater_root/trust_roots.json`.
    pub trust_roots: Option<&'a [TrustRoot]>,
    /// Host platform string compared to metadata `platform`.
    pub host_platform: &'a str,
    /// Unix seconds used for not_before / not_after checks.
    pub now_unix: i64,
    /// Relative smoke command under the staged release, if any.
    pub smoke_command: Option<&'a str>,
    /// Arguments passed to the smoke command after the program path.
    pub smoke_args: &'a [&'a str],
    /// Must be true to journal prepare + commit selector/floor.
    pub owner_authorized: bool,
}

/// Recover interrupted state, verify, fetch, stage, smoke, and optionally activate.
pub fn apply_update(options: ApplyOptions<'_>) -> Result<ApplyResult, UpdaterError> {
    let root = options.updater_root;
    assert_no_session_or_secret_reads(root)?;
    let recovered_before = recover_activation(root)?;
    let floor = AcceptedFloor {
        sequence: recovered_before.accepted_floor,
    };

    let roots_owned;
    let roots: &[TrustRoot] = if let Some(roots) = options.trust_roots {
        roots
    } else {
        roots_owned = load_trust_roots(&layout(root).trust_roots)?;
        &roots_owned
    };

    let verified = verify_release_metadata(
        options.metadata,
        roots,
        &floor,
        options.now_unix,
        options.host_platform,
    )?;
    assert_tcb_outside_candidate(root, verified.sequence)?;

    let package_dir = resolve_package_source(options.package_source)?;
    let fetched = fetch_artifacts_from_dir(&package_dir, &verified)?;
    let artifacts = fetched
        .into_iter()
        .map(|item| (item.name, item.bytes))
        .collect::<Vec<_>>();
    let staged = stage_verified_release(root, &verified, &artifacts)?;

    if let Some(command) = options.smoke_command {
        smoke_staged_release(&staged, command, options.smoke_args)?;
    }

    let activated = if options.owner_authorized {
        let previous = read_selector(root)?.current_sequence;
        journal_prepare(root, verified.sequence, previous)?;
        Some(commit_activation(root, verified.sequence)?)
    } else {
        // Staged-only path: candidate is immutable and floor does not advance.
        None
    };

    Ok(ApplyResult {
        verified,
        staged,
        activated,
        recovered_before,
    })
}

/// Discard a staged candidate that was never committed (failed smoke / opt-out).
///
/// Refuses to remove the currently selected generation or any sequence at or
/// below the accepted floor.
pub fn discard_staged_release(root: &Path, sequence: u64) -> Result<(), UpdaterError> {
    let selector = read_selector(root)?;
    if sequence == 0 {
        return Err(UpdaterError::InvalidMetadata(
            "cannot discard sequence 0".to_string(),
        ));
    }
    if sequence == selector.current_sequence {
        return Err(UpdaterError::InvalidMetadata(format!(
            "cannot discard currently selected sequence {sequence}"
        )));
    }
    if sequence <= selector.accepted_floor {
        return Err(UpdaterError::InvalidMetadata(format!(
            "cannot discard sequence {sequence} at or below accepted floor {}",
            selector.accepted_floor
        )));
    }
    let dir = crate::layout::release_directory(root, sequence);
    if !dir.exists() {
        return Err(UpdaterError::InvalidMetadata(format!(
            "staged release {sequence} not found"
        )));
    }
    fs::remove_dir_all(&dir).map_err(|error| {
        UpdaterError::InvalidMetadata(format!("discard staged release {}: {error}", dir.display()))
    })?;
    Ok(())
}
