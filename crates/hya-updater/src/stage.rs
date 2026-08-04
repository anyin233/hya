use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::UpdaterError;
use crate::layout::{assert_tcb_outside_candidate, release_directory};
use crate::metadata::VerifiedRelease;
use crate::verify::verify_artifact_bytes;

/// Immutable staged candidate under `root/releases/<sequence>/`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedRelease {
    pub sequence: u64,
    pub root: PathBuf,
}

impl StagedRelease {
    pub fn directory(&self) -> PathBuf {
        release_directory(&self.root, self.sequence)
    }
}

/// Stage verified artifacts into an immutable versioned directory.
///
/// Writes only under `root/releases/<sequence>/` and never mutates an existing
/// staged generation. Does not load candidate code as a library.
pub fn stage_verified_release(
    root: &Path,
    verified: &VerifiedRelease,
    artifacts: &[(String, Vec<u8>)],
) -> Result<StagedRelease, UpdaterError> {
    assert_tcb_outside_candidate(root, verified.sequence)?;
    if artifacts.len() != verified.artifacts.len() {
        return Err(UpdaterError::InvalidMetadata(
            "staged artifact count must match verified release".to_string(),
        ));
    }
    let staged = StagedRelease {
        sequence: verified.sequence,
        root: root.to_path_buf(),
    };
    let dir = staged.directory();
    if dir.exists() {
        return Err(UpdaterError::InvalidMetadata(format!(
            "release sequence {} already staged",
            verified.sequence
        )));
    }
    fs::create_dir_all(&dir).map_err(|error| {
        UpdaterError::InvalidMetadata(format!("create stage directory: {error}"))
    })?;

    for (name, bytes) in artifacts {
        // Reject path escape so staging cannot overwrite TCB control files.
        if name.is_empty()
            || name.starts_with('/')
            || name.starts_with('\\')
            || name.split(['/', '\\']).any(|part| part == "..")
        {
            return Err(UpdaterError::InvalidMetadata(format!(
                "artifact name `{name}` must be a relative path without `..`"
            )));
        }
        verify_artifact_bytes(verified, name, bytes)?;
        let path = dir.join(name);
        if path.exists() {
            return Err(UpdaterError::InvalidMetadata(format!(
                "artifact `{name}` already present in stage"
            )));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                UpdaterError::InvalidMetadata(format!(
                    "create artifact parent for `{name}`: {error}"
                ))
            })?;
        }
        let mut file = fs::File::create(&path).map_err(|error| {
            UpdaterError::InvalidMetadata(format!("create artifact `{name}`: {error}"))
        })?;
        file.write_all(bytes).map_err(|error| {
            UpdaterError::InvalidMetadata(format!("write artifact `{name}`: {error}"))
        })?;
        file.sync_all().map_err(|error| {
            UpdaterError::InvalidMetadata(format!("fsync artifact `{name}`: {error}"))
        })?;
        drop(file);
        // Release packages are meant to be smoke-tested and selected for boot.
        // Preserve a runnable mode on Unix; ownership remains root-level TCB.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&path)
                .map_err(|error| {
                    UpdaterError::InvalidMetadata(format!(
                        "stat artifact `{name}` for mode: {error}"
                    ))
                })?
                .permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).map_err(|error| {
                UpdaterError::InvalidMetadata(format!(
                    "set executable mode on artifact `{name}`: {error}"
                ))
            })?;
        }
    }
    // Parent fsync is best-effort; File::sync_all on directory is not portable.
    // Presence of complete artifact set is the staging completeness check.
    for expected in &verified.artifacts {
        if !dir.join(&expected.name).is_file() {
            return Err(UpdaterError::InvalidMetadata(format!(
                "missing staged artifact `{}`",
                expected.name
            )));
        }
    }
    Ok(staged)
}
