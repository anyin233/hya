//! Independent verify, stage, smoke, and owner-authorized activation pipeline.
use crate::error::UpdaterError;
use crate::fetch::{fetch_artifacts_from_dir, resolve_package_source};
use crate::journal::{
    ActivationSelector, commit_activation_owned, journal_prepare_owned, read_selector,
    recover_activation_locked,
};
use crate::layout::{assert_no_session_or_secret_reads, assert_tcb_outside_candidate, layout};
use crate::lease::ActivationAuthorization;
use crate::metadata::{AcceptedFloor, ReleaseMetadata, TrustRoot, VerifiedRelease};
use crate::smoke::smoke_staged_release;
use crate::stage::{StagedRelease, stage_verified_release};
use crate::trust::load_trust_roots;
use crate::verify::verify_release_metadata;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
/// Result of verification, staging, and optional activation.
pub struct ApplyResult {
    /// Verified signed release metadata.
    pub verified: VerifiedRelease,
    /// Filesystem staging result.
    pub staged: StagedRelease,
    /// Activated selector, or `None` for stage-only.
    pub activated: Option<ActivationSelector>,
    /// Selector recovered before applying this release.
    pub recovered_before: ActivationSelector,
}

#[derive(Clone, Debug)]
/// Inputs to the updater verification and staging pipeline.
pub struct ApplyOptions<'a> {
    /// Updater trust-boundary root.
    pub updater_root: &'a Path,
    /// Signed release metadata.
    pub metadata: &'a ReleaseMetadata,
    /// Local package directory or `file://` source.
    pub package_source: &'a str,
    /// Optional external trust roots.
    pub trust_roots: Option<&'a [TrustRoot]>,
    /// Platform string required by the metadata.
    pub host_platform: &'a str,
    /// Verification time in Unix seconds.
    pub now_unix: i64,
    /// Relative smoke command, when configured.
    pub smoke_command: Option<&'a str>,
    /// Arguments passed to the smoke command.
    pub smoke_args: &'a [&'a str],
    /// Capability issued by the trusted owner. `None` always means stage-only.
    pub activation: Option<&'a ActivationAuthorization>,
}

/// Verify, stage, smoke-test, and optionally activate one signed release.
pub fn apply_update(options: ApplyOptions<'_>) -> Result<ApplyResult, UpdaterError> {
    let root = options.updater_root;
    assert_no_session_or_secret_reads(root)?;
    let _lease = crate::lease::acquire(root)?;
    let recovered_before = recover_activation_locked(root)?;
    if let Some(auth) = options.activation {
        let issued = crate::lease::read_issuance(root)?;
        if issued.as_ref() != Some(auth)
            || auth.candidate_sequence() != options.metadata.sequence
            || auth.expected_generation() != recovered_before.generation
        {
            return Err(UpdaterError::StaleOwnerGeneration);
        }
    }
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
    let artifacts = fetch_artifacts_from_dir(&package_dir, &verified)?
        .into_iter()
        .map(|item| (item.name, item.bytes))
        .collect::<Vec<_>>();
    let staged = stage_verified_release(root, &verified, &artifacts)?;
    if let Some(command) = options.smoke_command {
        smoke_staged_release(&staged, command, options.smoke_args)?;
    }
    let activated = if let Some(auth) = options.activation {
        let previous = read_selector(root)?;
        if previous.generation != auth.expected_generation() {
            return Err(UpdaterError::StaleOwnerGeneration);
        }
        journal_prepare_owned(
            root,
            verified.sequence,
            previous.current_sequence,
            auth.owner_token(),
            auth.expected_generation(),
        )?;
        Some(commit_activation_owned(
            root,
            verified.sequence,
            auth.owner_token(),
            auth.expected_generation(),
        )?)
    } else {
        None
    };
    Ok(ApplyResult {
        verified,
        staged,
        activated,
        recovered_before,
    })
}

/// Remove an unaccepted candidate under the same root mutation lease as activation.
pub fn discard_staged_release(root: &Path, sequence: u64) -> Result<(), UpdaterError> {
    let _lease = crate::lease::acquire(root)?;
    let selector = read_selector(root)?;
    if sequence == 0 {
        return Err(UpdaterError::InvalidMetadata(
            "cannot discard sequence 0".into(),
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
    })
}
