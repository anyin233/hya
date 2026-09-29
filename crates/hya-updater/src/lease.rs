use crate::error::UpdaterError;
use crate::layout::layout;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::Path;

/// Capability issued by the trusted owner for exactly one candidate and active generation.
///
/// The capability is not a host-wide secret: same-UID processes sharing write access to
/// the updater root are in the same trust boundary. It prevents accidental or stale
/// activation and binds an owner decision to the exact candidate and generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivationAuthorization {
    candidate_sequence: u64,
    expected_generation: u64,
    owner_token: String,
}

impl ActivationAuthorization {
    pub(crate) fn candidate_sequence(&self) -> u64 {
        self.candidate_sequence
    }
    pub(crate) fn expected_generation(&self) -> u64 {
        self.expected_generation
    }
    pub(crate) fn owner_token(&self) -> &str {
        &self.owner_token
    }
}

/// Root lease held by the trusted updater owner while issuing authorization.
pub struct UpdaterOwner {
    /// Held until drop to retain the root flock.
    #[allow(dead_code)]
    lease: File,
    token: String,
}

impl UpdaterOwner {
    /// Acquire exclusive ownership of the updater root and bind a candidate decision.
    pub fn acquire(root: &Path) -> Result<Self, UpdaterError> {
        let lease = acquire_file(root)?;
        Ok(Self {
            lease,
            token: format!("{}-{}", std::process::id(), monotonic_nonce()),
        })
    }

    /// Authorize only the exact candidate and currently observed active generation.
    pub fn authorize(
        &self,
        root: &Path,
        candidate_sequence: u64,
        expected_generation: u64,
    ) -> Result<ActivationAuthorization, UpdaterError> {
        let current = crate::journal::read_selector(root)?;
        if current.generation != expected_generation {
            return Err(UpdaterError::StaleOwnerGeneration);
        }
        let authorization = ActivationAuthorization {
            candidate_sequence,
            expected_generation,
            owner_token: self.token.clone(),
        };
        persist_issuance(root, &authorization)?;
        Ok(authorization)
    }

    /// Persist a capability for the explicit CLI handoff. The file is one-shot by generation CAS.
    pub fn write_authorization(
        &self,
        path: &Path,
        authorization: &ActivationAuthorization,
    ) -> Result<(), UpdaterError> {
        if authorization.owner_token != self.token {
            return Err(UpdaterError::StaleOwnerGeneration);
        }
        let body = serde_json::to_vec(authorization)
            .map_err(|e| UpdaterError::InvalidMetadata(format!("serialize authorization: {e}")))?;
        let mut file = File::create(path)
            .map_err(|e| UpdaterError::InvalidMetadata(format!("create authorization: {e}")))?;
        file.write_all(&body)
            .map_err(|e| UpdaterError::InvalidMetadata(format!("write authorization: {e}")))?;
        file.sync_all()
            .map_err(|e| UpdaterError::InvalidMetadata(format!("sync authorization: {e}")))
    }

    /// Record an owner-authorized activation prepare entry.
    pub fn prepare_activation(
        &self,
        root: &Path,
        authorization: &ActivationAuthorization,
        previous_sequence: u64,
    ) -> Result<(), UpdaterError> {
        if authorization.owner_token != self.token {
            return Err(UpdaterError::StaleOwnerGeneration);
        }
        crate::journal::journal_prepare_owned(
            root,
            authorization.candidate_sequence,
            previous_sequence,
            authorization.owner_token(),
            authorization.expected_generation(),
        )
    }

    /// Commit the exact capability's prepared candidate.
    pub fn commit_activation(
        &self,
        root: &Path,
        authorization: &ActivationAuthorization,
    ) -> Result<crate::journal::ActivationSelector, UpdaterError> {
        if authorization.owner_token != self.token {
            return Err(UpdaterError::StaleOwnerGeneration);
        }
        crate::journal::commit_activation_owned(
            root,
            authorization.candidate_sequence,
            authorization.owner_token(),
            authorization.expected_generation(),
        )
    }
}

/// Internal mutation lease. No activation capability is minted here.
pub(crate) struct UpdaterLease {
    /// Held until drop to retain the mutation flock.
    #[allow(dead_code)]
    pub(crate) file: File,
}

pub(crate) fn acquire(root: &Path) -> Result<UpdaterLease, UpdaterError> {
    Ok(UpdaterLease {
        file: acquire_file(root)?,
    })
}

fn acquire_file(root: &Path) -> Result<File, UpdaterError> {
    std::fs::create_dir_all(root)
        .map_err(|e| UpdaterError::InvalidMetadata(format!("create updater root: {e}")))?;
    let path = layout(root).lease;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| UpdaterError::LeaseUnavailable(format!("open updater lease: {e}")))?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        return Err(UpdaterError::LeaseUnavailable(
            "updater root is already owned".into(),
        ));
    }
    Ok(file)
}

fn monotonic_nonce() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn persist_issuance(
    root: &Path,
    authorization: &ActivationAuthorization,
) -> Result<(), UpdaterError> {
    let path = layout(root).authorization;
    let body = serde_json::to_vec(authorization)
        .map_err(|e| UpdaterError::InvalidMetadata(format!("serialize authorization: {e}")))?;
    std::fs::write(&path, body)
        .map_err(|e| UpdaterError::InvalidMetadata(format!("write authorization: {e}")))
}

pub(crate) fn read_issuance(root: &Path) -> Result<Option<ActivationAuthorization>, UpdaterError> {
    let path = layout(root).authorization;
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| UpdaterError::InvalidMetadata(format!("parse authorization: {e}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(UpdaterError::InvalidMetadata(format!(
            "read authorization: {error}"
        ))),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_root_lease_is_rejected() {
        let root = std::env::temp_dir().join(format!("hya-updater-lease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let first = match UpdaterOwner::acquire(&root) {
            Ok(owner) => owner,
            Err(error) => panic!("lease acquisition failed: {error}"),
        };
        assert!(matches!(
            UpdaterOwner::acquire(&root),
            Err(UpdaterError::LeaseUnavailable(_))
        ));
        drop(first);
        let _ = std::fs::remove_dir_all(root);
    }
}
