//! Database path identity and exclusive runtime ownership.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::{OwnerRunId, SessionStore, StoreError};

pub(super) struct RuntimeOwnerState {
    paths: Option<DatabasePaths>,
    claim: Mutex<Option<RuntimeOwnerClaim>>,
    pub(super) pending_lock: Mutex<Option<RuntimeOwnerLock>>,
}

struct RuntimeOwnerClaim {
    owner: OwnerRunId,
    _lock_file: Option<RuntimeOwnerLock>,
}

pub(super) struct RuntimeOwnerLock(File);

impl Drop for RuntimeOwnerLock {
    fn drop(&mut self) {
        let _ = self.0.set_len(0);
    }
}

impl RuntimeOwnerState {
    pub(super) fn memory() -> Self {
        Self {
            paths: None,
            claim: Mutex::new(None),
            pending_lock: Mutex::new(None),
        }
    }

    pub(super) fn file(paths: DatabasePaths) -> Self {
        Self {
            paths: Some(paths),
            claim: Mutex::new(None),
            pending_lock: Mutex::new(None),
        }
    }

    fn claim(&self, owner: OwnerRunId) -> Result<(), StoreError> {
        let mut claim = self
            .claim
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = claim.as_ref() {
            return if existing.owner == owner {
                Ok(())
            } else {
                Err(StoreError::RuntimeOwnerBusy)
            };
        }

        let lock_file = match self
            .pending_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            Some(file) => Some(file),
            None => self
                .paths
                .as_ref()
                .map(acquire_runtime_owner_lock)
                .transpose()?,
        };
        *claim = Some(RuntimeOwnerClaim {
            owner,
            _lock_file: lock_file,
        });
        Ok(())
    }

    pub(super) fn current_owner(&self) -> Result<OwnerRunId, StoreError> {
        self.claim
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|claim| claim.owner)
            .ok_or(StoreError::RuntimeOwnerClaimRequired)
    }

    fn require(&self, owner: OwnerRunId) -> Result<(), StoreError> {
        let claim = self
            .claim
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match claim.as_ref() {
            Some(claim) if claim.owner == owner => Ok(()),
            _ => Err(StoreError::RuntimeOwnerClaimRequired),
        }
    }
    fn release(&self, owner: OwnerRunId) -> Result<(), StoreError> {
        let mut claim = self
            .claim
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match claim.as_ref() {
            Some(existing) if existing.owner == owner => {
                *claim = None;
                Ok(())
            }
            Some(_) => Err(StoreError::RuntimeOwnerBusy),
            None => Ok(()),
        }
    }
}

/// Canonical database identity and its adjacent ownership/discovery files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabasePaths {
    /// SQLite file, resolving symlinks even when their target does not exist yet.
    pub database: PathBuf,
    /// Sole exclusive runtime-owner lock; its content is the owner's PID.
    pub lock: PathBuf,
    /// Published daemon URL and PID.
    pub discovery: PathBuf,
    /// Detached daemon output.
    pub log: PathBuf,
    /// Stop/restart notice addressed to the current owner.
    pub stop: PathBuf,
    /// Persistent relay identity.
    pub relay_identity: PathBuf,
}

/// Resolve a plain database filename and all sidecars using one identity.
/// Only `""` and `":memory:"` select memory. All database URIs are rejected:
/// runtime writers must use a plain path with a canonical lock identity.
/// Validate only the lexical database argument form (`""`, `":memory:"`, or a
/// plain filesystem path) without performing any filesystem syscalls.
///
/// # Errors
/// Returns [`std::io::ErrorKind::InvalidInput`] for `file:` or `sqlite:` URIs.
pub fn validate_database_arg(db: &str) -> std::io::Result<()> {
    if db.starts_with("file:") || db.starts_with("sqlite:") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "database URIs are not supported; use a plain filesystem path or :memory:",
        ));
    }
    Ok(())
}

/// Canonical filesystem paths for `db`, or `None` for in-memory spellings
/// (`""` and `":memory:"`).
pub fn database_paths(db: &str) -> std::io::Result<Option<DatabasePaths>> {
    validate_database_arg(db)?;
    if db.is_empty() || db == ":memory:" {
        return Ok(None);
    }
    let database = resolve_database_path(Path::new(db), &mut 40)?;
    let with = |suffix: &str| {
        let mut path = database.as_os_str().to_owned();
        path.push(suffix);
        PathBuf::from(path)
    };
    Ok(Some(DatabasePaths {
        lock: with(".runtime-owner.lock"),
        discovery: with(".server.json"),
        log: with(".server.log"),
        stop: with(".server.stop"),
        relay_identity: with(".relay-identity.json"),
        database,
    }))
}

fn resolve_database_path(path: &Path, links_left: &mut usize) -> std::io::Result<PathBuf> {
    let mut resolved = if path.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir()?
    };
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::RootDir | Component::Prefix(_) => resolved.push(component.as_os_str()),
            Component::Normal(name) => {
                resolved.push(name);
                match fs::symlink_metadata(&resolved) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        if *links_left == 0 {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "too many database symlinks",
                            ));
                        }
                        *links_left -= 1;
                        let target = fs::read_link(&resolved)?;
                        resolved.pop();
                        resolved = resolve_database_path(&resolved.join(target), links_left)?;
                    }
                    Ok(_) => resolved = fs::canonicalize(&resolved)?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(resolved)
}

pub(super) fn runtime_owner_lock_error(path: Option<&Path>, source: std::io::Error) -> StoreError {
    StoreError::RuntimeOwnerLock {
        path: path.map_or_else(|| PathBuf::from("<memory>"), Path::to_path_buf),
        source: Arc::new(source),
    }
}

pub(super) fn acquire_runtime_owner_lock(
    paths: &DatabasePaths,
) -> Result<RuntimeOwnerLock, StoreError> {
    let path = &paths.lock;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(runtime_owner_lock_error(
                Some(path),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "runtime owner lock path must not be a symbolic link",
                ),
            ));
        }
        Ok(_) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(runtime_owner_lock_error(Some(path), source)),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|source| runtime_owner_lock_error(Some(path), source))?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| runtime_owner_lock_error(Some(path), source))?;
    // Discovery briefly probes this same lock. Give such a probe time to
    // release it rather than reporting a false competing runtime.
    let deadline = std::time::Instant::now() + Duration::from_millis(200);
    loop {
        match file.try_lock() {
            Ok(()) => {
                let lock = RuntimeOwnerLock(file);
                lock.0
                    .set_len(0)
                    .map_err(|source| runtime_owner_lock_error(Some(path), source))?;
                // Clear notices before publishing the PID: requests addressed to
                // this owner after publication must survive the rest of startup.
                for stale in [&paths.stop, &paths.discovery] {
                    match fs::remove_file(stale) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(runtime_owner_lock_error(Some(stale), error)),
                    }
                }
                writeln!(&lock.0, "{}", std::process::id())
                    .map_err(|source| runtime_owner_lock_error(Some(path), source))?;
                return Ok(lock);
            }
            Err(TryLockError::WouldBlock) => {
                // Shared probes can delay acquisition; an exclusive owner is
                // a real competitor and must be rejected immediately.
                match file.try_lock_shared() {
                    Ok(()) => file
                        .unlock()
                        .map_err(|source| runtime_owner_lock_error(Some(path), source))?,
                    Err(TryLockError::WouldBlock) => return Err(StoreError::RuntimeOwnerBusy),
                    Err(TryLockError::Error(source)) => {
                        return Err(runtime_owner_lock_error(Some(path), source));
                    }
                }
                if std::time::Instant::now() >= deadline {
                    return Err(StoreError::RuntimeOwnerBusy);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(TryLockError::Error(source)) => {
                return Err(runtime_owner_lock_error(Some(path), source));
            }
        }
    }
}

impl SessionStore {
    /// On-disk database and runtime files, when this store is file-backed.
    pub fn paths(&self) -> Option<&DatabasePaths> {
        self.runtime_owner.paths.as_ref()
    }

    /// Claim this store as the runtime owner for startup recovery and Workflow control.
    ///
    /// File-backed stores hold an exclusive lock next to the canonical SQLite file
    /// until explicitly released after drain or every clone of this store is dropped.
    /// In-memory stores retain the
    /// owner identity in the store instance. Repeating the claim with the same owner
    /// is idempotent; a different owner is rejected while the claim is held.
    pub fn claim_runtime_owner(&self, owner: OwnerRunId) -> Result<(), StoreError> {
        self.runtime_owner.claim(owner)
    }
    /// Release this store's runtime-owner claim before a successor opens it.
    /// The caller must have quiesced all writes first.
    pub fn release_runtime_owner(&self, owner: OwnerRunId) -> Result<(), StoreError> {
        self.runtime_owner.release(owner)
    }

    /// Require a matching runtime-owner claim before a startup-only mutation.
    pub(crate) fn require_runtime_owner(&self, owner: OwnerRunId) -> Result<(), StoreError> {
        self.runtime_owner.require(owner)
    }
}
