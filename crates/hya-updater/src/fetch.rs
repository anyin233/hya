//! Local package fetch for the updater TCB.
//!
//! Network download stays outside the TCB (CI or an operator downloads a
//! complete package directory). This module only copies verified-named paths
//! from a local package tree into memory for staging.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::UpdaterError;
use crate::metadata::VerifiedRelease;

/// One artifact payload loaded from a local package directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FetchedArtifact {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Resolve a package source path.
///
/// Accepts ordinary filesystem paths or `file://` URLs. Remote schemes are
/// rejected so the TCB never opens the network itself.
pub fn resolve_package_source(source: &str) -> Result<PathBuf, UpdaterError> {
    if source.is_empty() {
        return Err(UpdaterError::InvalidMetadata(
            "package source must be non-empty".to_string(),
        ));
    }
    if let Some(rest) = source.strip_prefix("file://") {
        if rest.is_empty() {
            return Err(UpdaterError::InvalidMetadata(
                "file:// URL must include a path".to_string(),
            ));
        }
        return Ok(PathBuf::from(rest));
    }
    if source.contains("://") {
        return Err(UpdaterError::InvalidMetadata(format!(
            "package source `{source}` uses an unsupported scheme; download outside the TCB and pass a local path or file:// URL"
        )));
    }
    Ok(PathBuf::from(source))
}

/// Load every artifact named by a verified release from `package_dir`.
///
/// Artifact names may not escape the package directory.
pub fn fetch_artifacts_from_dir(
    package_dir: &Path,
    verified: &VerifiedRelease,
) -> Result<Vec<FetchedArtifact>, UpdaterError> {
    if !package_dir.is_dir() {
        return Err(UpdaterError::InvalidMetadata(format!(
            "package directory missing: {}",
            package_dir.display()
        )));
    }
    let mut out = Vec::with_capacity(verified.artifacts.len());
    for artifact in &verified.artifacts {
        let name = &artifact.name;
        if name.is_empty()
            || name.starts_with('/')
            || name.starts_with('\\')
            || name.split(['/', '\\']).any(|part| part == "..")
        {
            return Err(UpdaterError::InvalidMetadata(format!(
                "artifact name `{name}` must be a relative path without `..`"
            )));
        }
        let path = package_dir.join(name);
        let bytes = fs::read(&path).map_err(|error| {
            UpdaterError::InvalidMetadata(format!(
                "read package artifact `{}` ({}): {error}",
                name,
                path.display()
            ))
        })?;
        out.push(FetchedArtifact {
            name: name.clone(),
            bytes,
        });
    }
    Ok(out)
}
