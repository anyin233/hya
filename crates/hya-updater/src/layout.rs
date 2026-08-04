//! Independent ownership layout for the updater TCB.
//!
//! Trust roots, accepted floor, activation journal, and selector live under the
//! updater root and must never sit inside a candidate release directory. The
//! candidate generation is only readable staging under `releases/<sequence>/`.

use std::path::{Path, PathBuf};

use crate::error::UpdaterError;

/// Paths that form the protected updater surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdaterLayout {
    pub root: PathBuf,
    pub trust_roots: PathBuf,
    pub accepted_floor: PathBuf,
    pub journal: PathBuf,
    pub selector: PathBuf,
    pub releases: PathBuf,
}

/// Build the canonical layout under an updater root.
pub fn layout(root: &Path) -> UpdaterLayout {
    UpdaterLayout {
        root: root.to_path_buf(),
        trust_roots: root.join("trust_roots.json"),
        accepted_floor: root.join("accepted_floor"),
        journal: root.join("activation.journal"),
        selector: root.join("current"),
        releases: root.join("releases"),
    }
}

/// Path of one immutable staged generation.
pub fn release_directory(root: &Path, sequence: u64) -> PathBuf {
    layout(root).releases.join(sequence.to_string())
}

/// Fail closed if a candidate path could contain or replace TCB control files.
///
/// This is a path-layout proof only: it does not claim OS sandbox isolation.
/// Production packaging still needs ownership/permissions on the host.
pub fn assert_tcb_outside_candidate(
    root: &Path,
    candidate_sequence: u64,
) -> Result<(), UpdaterError> {
    let layout = layout(root);
    let candidate = release_directory(root, candidate_sequence);
    let protected = [
        layout.trust_roots.as_path(),
        layout.accepted_floor.as_path(),
        layout.journal.as_path(),
        layout.selector.as_path(),
    ];
    for path in protected {
        if path.starts_with(&candidate) {
            return Err(UpdaterError::OwnershipViolation(format!(
                "protected path `{}` must not live under candidate `{}`",
                path.display(),
                candidate.display()
            )));
        }
        // TCB control files live at the root, not inside any release tree.
        if path
            .strip_prefix(root)
            .map(|rel| rel.starts_with("releases"))
            .unwrap_or(false)
        {
            return Err(UpdaterError::OwnershipViolation(format!(
                "protected path `{}` must not be under releases/",
                path.display()
            )));
        }
    }
    if layout.releases != root.join("releases") {
        return Err(UpdaterError::OwnershipViolation(
            "releases directory must be root/releases".to_string(),
        ));
    }
    Ok(())
}

/// Confirm the updater root does not expose session DB or runtime secret paths.
pub fn assert_no_session_or_secret_reads(root: &Path) -> Result<(), UpdaterError> {
    let forbidden_names = [
        "sessions.sqlite",
        "sessions.db",
        "session.db",
        "auth.json",
        "credentials",
        "secrets",
    ];
    for name in forbidden_names {
        let path = root.join(name);
        if path.exists() {
            return Err(UpdaterError::OwnershipViolation(format!(
                "updater root must not contain runtime secret/session path `{}`",
                path.display()
            )));
        }
    }
    Ok(())
}
