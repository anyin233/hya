//! `hya-store` — SQLite event log + replay; projection folded on read via the
//! shared `hya_proto::Projection` reducer (one reducer, no SQL/reducer
//! divergence). Folds are cached in-process and as durable snapshots
//! (`projection_cache`), a pure cache of that reducer: a read folds only the
//! events after the cached projection and always equals a full replay.
//!
//! NOTE: PRAGMAs (WAL etc.) are set via connect options, NOT a migration — `WAL`
//! cannot run inside the transaction sqlx wraps migrations in.

#[cfg(feature = "lifecycle-bench")]
pub mod bench;

mod agent_effort_preference;
mod agent_model_preference;
/// Typed store errors for the session store.
pub mod error;
mod file_blob;
mod handoff;
mod mailbox;
mod materialize;
mod model_effort_preference;
mod paths;
mod permission;
mod project;
mod projection_cache;
mod recovery;
mod resident_claim;
mod workflow;
mod write_queue;

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hya_proto::{Envelope, Event, EventSeq, ProjectId, Projection, SessionId, now_millis};
use projection_cache::ProjectionCache;
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub use agent_effort_preference::AgentEffortPreference;
pub use agent_model_preference::AgentModelPreference;
pub use error::StoreError;
pub use handoff::PendingResume;
pub use hya_proto::{ActorClaim, OwnerRunId};
pub use mailbox::{
    RecoveredResidentOutcome, RecoveredResidentWork, ResidentArchive, ResidentArchiveOutcome,
    ResidentArchiveOwner,
};
pub use model_effort_preference::ModelEffortPreference;
pub use paths::user_cache_dir;
pub use permission::SavedPermission;
pub use project::{Project, ProjectSummary, normalize_project_path};
pub use recovery::{INTERRUPTED_REASON, InterruptedTurnRecovery};
pub use resident_claim::{EpisodeRegistration, RecoveredActorClaim, ResidentWorkPrompt};
pub use workflow::{WorkflowAdmissionOutcome, WorkflowSelectionOutcome};
pub use write_queue::{
    EXIT_DRAIN_TIMEOUT, ExitDrainReport, OPEN_TIMEOUT, READ_TIMEOUT, StoreHealth, WRITE_TIMEOUT,
};

// A busy disk can delay both pool acquisition and release of SQLite locks.
const FILE_POOL_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

struct RuntimeOwnerState {
    paths: Option<DatabasePaths>,
    claim: Mutex<Option<RuntimeOwnerClaim>>,
    pending_lock: Mutex<Option<RuntimeOwnerLock>>,
}

struct RuntimeOwnerClaim {
    owner: OwnerRunId,
    _lock_file: Option<RuntimeOwnerLock>,
}

struct RuntimeOwnerLock(File);

impl Drop for RuntimeOwnerLock {
    fn drop(&mut self) {
        let _ = self.0.set_len(0);
    }
}

impl RuntimeOwnerState {
    fn memory() -> Self {
        Self {
            paths: None,
            claim: Mutex::new(None),
            pending_lock: Mutex::new(None),
        }
    }

    fn file(paths: DatabasePaths) -> Self {
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

fn runtime_owner_lock_error(path: Option<&Path>, source: std::io::Error) -> StoreError {
    StoreError::RuntimeOwnerLock {
        path: path.map_or_else(|| PathBuf::from("<memory>"), Path::to_path_buf),
        source: Arc::new(source),
    }
}

pub(crate) async fn run_on_detached_thread<F, T>(f: F) -> Result<T, StoreError>
where
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("hya-store-io".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|err| StoreError::from(sqlx::Error::Io(err)))?;
    rx.await
        .unwrap_or_else(|_| Err(StoreError::from(sqlx::Error::WorkerCrashed)))
}

#[cfg(test)]
static TEST_STALL_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn is_open_storage_fault(error: &StoreError) -> bool {
    match error {
        StoreError::RuntimeOwnerBusy | StoreError::UnsupportedDatabase => false,
        StoreError::RuntimeOwnerLock { source, .. } => {
            source.kind() != std::io::ErrorKind::InvalidInput
        }
        StoreError::Migrate(err) => match err.as_ref() {
            sqlx::migrate::MigrateError::Execute(sqlx_err)
            | sqlx::migrate::MigrateError::ExecuteMigration(sqlx_err, _) => {
                error::is_sqlite_storage_fault(sqlx_err)
            }
            _ => false,
        },
        other => other.is_storage_fault(),
    }
}

fn acquire_runtime_owner_lock(paths: &DatabasePaths) -> Result<RuntimeOwnerLock, StoreError> {
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

#[derive(Clone, Debug)]
struct MemoryFallbackInfo {
    path: PathBuf,
    reason: String,
}

/// SQLite-backed session event log, resident claims, and related tables.
///
/// Construct with [`SessionStore::connect`] (file) or [`SessionStore::connect_memory`].
/// Projection is folded on read via `hya_proto::Projection` — there is no separate
/// materialized read model; folds are cached (in-process and as durable
/// snapshots) as a pure cache of that reducer, shared by every clone.
#[derive(Clone)]
pub struct SessionStore {
    writer_pool: sqlx::SqlitePool,
    read_pool: sqlx::SqlitePool,
    write_queue: Option<write_queue::WriteQueue>,
    read_timeout: Duration,
    runtime_owner: Arc<RuntimeOwnerState>,
    projections: Arc<ProjectionCache>,
    read_only: bool,
    memory_fallback: Option<Arc<MemoryFallbackInfo>>,
    exit_reported: Arc<std::sync::atomic::AtomicBool>,
    /// In-memory stores only: an idle connection that keeps the database
    /// alive. SQLite drops an in-memory database with its last connection,
    /// and the pool closes its one connection when a query future is dropped
    /// mid-flight (a cancelled turn), reconnecting to a fresh, empty database.
    _memory_keeper: Option<Arc<Mutex<sqlx::SqliteConnection>>>,
}

/// One session row from `list_sessions`: id, time bounds, and event count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    /// Session identity.
    pub session: SessionId,
    /// Earliest event timestamp in the log (unix millis).
    pub started_millis: i64,
    /// Latest event timestamp in the log (unix millis).
    pub updated_millis: i64,
    /// Number of rows in `event_log` for this session.
    pub events: u64,
}
/// One durable file activity row extracted from a `FilesChanged` event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectActivityFile {
    /// Changed path.
    pub path: String,
    /// Session that changed it.
    pub session: SessionId,
    /// Event timestamp.
    pub changed_millis: i64,
    /// Whether the path was absent before the change.
    pub created: bool,
}

impl SessionStore {
    /// On-disk database and runtime files, when this store is file-backed.
    pub fn paths(&self) -> Option<&DatabasePaths> {
        self.runtime_owner.paths.as_ref()
    }

    /// Open a plain filesystem path (initialize the schema and enable WAL), or
    /// `""` / `":memory:"` with the keeper-backed in-memory mode.
    ///
    /// This does not acquire the exclusive runtime-owner lock and must not be
    /// used to open a second concurrent appender on the same database file
    /// (in-process sequence numbering assumes one appender process; a second
    /// writer would trigger a foreign-writer conflict halt). Runtime writers
    /// use [`Self::connect_runtime`] / [`Self::connect_runtime_or_memory`] and
    /// [`Self::claim_runtime_owner`]. Existing databases without the current
    /// baseline return [`StoreError::UnsupportedDatabase`].
    pub async fn connect(path: &str) -> Result<Self, StoreError> {
        Self::connect_with_timeouts(path, WRITE_TIMEOUT, READ_TIMEOUT).await
    }

    fn writer_pool_options() -> SqlitePoolOptions {
        use sqlx::TransactionManager as _;
        SqlitePoolOptions::new()
            .max_connections(1)
            // Skip pre-acquire ping: cancelling a pending acquire mid-ping drops the popped
            // `Floating<Idle>` handle and closes the sole writer (triggering WAL checkpoint if
            // no readers are open). `after_release` and sqlx's release ping validate on drop.
            .test_before_acquire(false)
            // Closing the last connection of a WAL database checkpoints under an exclusive lock.
            .max_lifetime(None)
            .idle_timeout(None)
            // Discard a pooled writer connection if SQLite auto-rolled back (e.g. SQLITE_FULL)
            // while sqlx's worker still considers a transaction open.
            .after_release(|conn, _| {
                Box::pin(async move {
                    use sqlx::Connection as _;
                    if <sqlx::Sqlite as sqlx::Database>::TransactionManager::get_transaction_depth(
                        conn,
                    ) == 0
                    {
                        return Ok(true);
                    }
                    // Flush queued worker commands (async `Transaction::drop` rollbacks
                    // and any `Command::Begin` from a cancelled `begin_with`).
                    conn.ping().await?;
                    if <sqlx::Sqlite as sqlx::Database>::TransactionManager::get_transaction_depth(
                        conn,
                    ) == 0
                    {
                        return Ok(true);
                    }
                    // Roll back an open transaction left by a cancelled `begin_with` without
                    // closing the pinned writer; if SQLite already auto-rolled back (e.g.
                    // ENOSPC), `ROLLBACK` fails and we discard the poisoned connection.
                    Ok(
                        <sqlx::Sqlite as sqlx::Database>::TransactionManager::rollback(conn)
                            .await
                            .is_ok(),
                    )
                })
            })
    }

    /// Current durability health of this store (`Healthy` or `Degraded`).
    #[must_use]
    pub fn health(&self) -> StoreHealth {
        match &self.write_queue {
            Some(queue) => queue.health(),
            None => StoreHealth::Healthy,
        }
    }

    /// Configured bound for fallback-guarded reads (`READ_TIMEOUT` in production).
    #[must_use]
    pub fn read_timeout(&self) -> Duration {
        self.read_timeout
    }

    /// Test-only helper to open a store with custom synchronous write and fallback read bounds.
    #[doc(hidden)]
    pub async fn connect_with_timeouts(
        path: &str,
        write_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<Self, StoreError> {
        let paths = database_paths(path)
            .map_err(|source| runtime_owner_lock_error(Some(Path::new(path)), source))?;
        match paths {
            Some(paths) => Self::connect_file(paths, None, write_timeout, read_timeout).await,
            None => Self::connect_memory().await,
        }
    }

    /// Test-only helper to open a store with custom synchronous write and read bounds.
    #[doc(hidden)]
    pub async fn connect_with_write_timeout(
        path: &str,
        timeout: Duration,
    ) -> Result<Self, StoreError> {
        Self::connect_with_timeouts(path, timeout, timeout).await
    }

    async fn connect_file(
        paths: DatabasePaths,
        lock: Option<RuntimeOwnerLock>,
        write_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<Self, StoreError> {
        let writer_opts = SqliteConnectOptions::new()
            .filename(&paths.database)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(FILE_POOL_WAIT_TIMEOUT)
            .foreign_keys(true);
        let writer_pool_options =
            Self::writer_pool_options().acquire_timeout(FILE_POOL_WAIT_TIMEOUT);
        #[cfg(feature = "lifecycle-bench")]
        let writer_pool_options =
            writer_pool_options.after_connect(|connection, _| Box::pin(bench::install(connection)));
        let writer_pool = writer_pool_options.connect_with(writer_opts).await?;
        Self::migrate(&writer_pool).await?;

        let read_opts = SqliteConnectOptions::new()
            .filename(&paths.database)
            .read_only(true)
            .create_if_missing(false)
            .busy_timeout(FILE_POOL_WAIT_TIMEOUT);
        let read_pool_options = SqlitePoolOptions::new()
            .max_connections(4)
            .acquire_timeout(FILE_POOL_WAIT_TIMEOUT)
            // Closing the last connection of a WAL database checkpoints under an exclusive lock.
            .max_lifetime(None)
            .idle_timeout(None);
        #[cfg(feature = "lifecycle-bench")]
        let read_pool_options =
            read_pool_options.after_connect(|connection, _| Box::pin(bench::install(connection)));
        let read_pool = read_pool_options.connect_lazy_with(read_opts);

        let projections = Arc::new(ProjectionCache::new());
        let write_queue = write_queue::WriteQueue::start(
            paths.database.clone(),
            writer_pool.clone(),
            Arc::clone(&projections),
            write_timeout,
        )
        .await?;
        let runtime_owner = RuntimeOwnerState::file(paths);
        *runtime_owner
            .pending_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = lock;
        Ok(Self {
            writer_pool,
            read_pool,
            write_queue: Some(write_queue),
            read_timeout,
            runtime_owner: Arc::new(runtime_owner),
            projections,
            read_only: false,
            memory_fallback: None,
            exit_reported: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            _memory_keeper: None,
        })
    }

    /// Reserve runtime ownership before SQLite initialization with [`OPEN_TIMEOUT`],
    /// then transfer the lock to the first `claim_runtime_owner` call on the
    /// returned store. Fails on timeout or error without falling back to memory.
    pub async fn connect_runtime(path: &str) -> Result<Self, StoreError> {
        Self::connect_runtime_bounded(path, OPEN_TIMEOUT, WRITE_TIMEOUT, READ_TIMEOUT)
            .await
            .map_err(|(error, _)| error)
    }

    async fn connect_runtime_bounded(
        path: &str,
        open_timeout: Duration,
        write_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<Self, (StoreError, PathBuf)> {
        let fallback_path = PathBuf::from(path);
        if let Err(source) = validate_database_arg(path) {
            return Err((
                runtime_owner_lock_error(Some(Path::new(path)), source),
                fallback_path,
            ));
        }
        if path.is_empty() || path == ":memory:" {
            return Self::connect_memory()
                .await
                .map_err(|err| (err, fallback_path));
        }
        let path_owned = path.to_string();
        let resolved_slot = Arc::new(Mutex::new(None));
        let resolved_for_thread = Arc::clone(&resolved_slot);
        let opened = tokio::time::timeout(open_timeout, async move {
            let (paths, lock) = run_on_detached_thread(move || {
                #[cfg(test)]
                if TEST_STALL_OPEN.load(std::sync::atomic::Ordering::Acquire) {
                    std::thread::park();
                }
                let paths = database_paths(&path_owned).map_err(|source| {
                    runtime_owner_lock_error(Some(Path::new(&path_owned)), source)
                })?;
                let Some(paths) = paths else {
                    return Err(runtime_owner_lock_error(
                        Some(Path::new(&path_owned)),
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "expected file-backed database path",
                        ),
                    ));
                };
                *resolved_for_thread
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(paths.database.clone());
                let lock = acquire_runtime_owner_lock(&paths)?;
                Ok((paths, lock))
            })
            .await?;
            Self::connect_file(paths, Some(lock), write_timeout, read_timeout).await
        })
        .await;
        let report_path = resolved_slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or(fallback_path);
        match opened {
            Ok(Ok(store)) => Ok(store),
            Ok(Err(err)) => Err((err, report_path)),
            Err(_) => Err((
                StoreError::StorageTimeout {
                    operation: "open",
                    timeout: open_timeout,
                },
                report_path,
            )),
        }
    }

    /// Open a runtime store with [`OPEN_TIMEOUT`], falling back to an in-memory
    /// store when opening exceeds the bound or fails with a storage fault.
    ///
    /// [`StoreError::RuntimeOwnerBusy`], [`StoreError::UnsupportedDatabase`],
    /// and invalid path/URI or symlink-lock errors always propagate rather than
    /// falling back to memory.
    pub async fn connect_runtime_or_memory(path: &str) -> Result<Self, StoreError> {
        Self::connect_runtime_or_memory_with_timeouts(
            path,
            OPEN_TIMEOUT,
            WRITE_TIMEOUT,
            READ_TIMEOUT,
        )
        .await
    }

    /// Test-only helper to open a runtime store or fall back to memory with
    /// custom open, synchronous write, and fallback read bounds.
    #[doc(hidden)]
    pub async fn connect_runtime_or_memory_with_timeouts(
        path: &str,
        open_timeout: Duration,
        write_timeout: Duration,
        read_timeout: Duration,
    ) -> Result<Self, StoreError> {
        match Self::connect_runtime_bounded(path, open_timeout, write_timeout, read_timeout).await {
            Ok(store) => Ok(store),
            Err((error, _)) if !is_open_storage_fault(&error) => Err(error),
            Err((error, report_path)) => {
                Self::connect_memory_fallback(report_path, error.to_string(), read_timeout).await
            }
        }
    }

    async fn connect_memory_fallback(
        path: PathBuf,
        reason: String,
        read_timeout: Duration,
    ) -> Result<Self, StoreError> {
        let mut store = Self::connect_memory().await?;
        store.read_timeout = read_timeout;
        store.memory_fallback = Some(Arc::new(MemoryFallbackInfo {
            path,
            reason: reason.replace(['\n', '\r'], " ").trim().to_string(),
        }));
        Ok(store)
    }

    /// Requested path and open failure reason when this store fell back to memory.
    #[must_use]
    pub fn memory_fallback(&self) -> Option<(&Path, &str)> {
        self.memory_fallback
            .as_deref()
            .map(|info| (info.path.as_path(), info.reason.as_str()))
    }

    /// Wait up to `timeout` for any queued appends to flush to disk and return
    /// an [`ExitDrainReport`] describing the store's final durability status.
    /// Returns immediately when the writer is halted or the store is an
    /// in-memory fallback.
    pub async fn drain_for_exit(&self, timeout: Duration) -> ExitDrainReport {
        if let Some(info) = &self.memory_fallback {
            let event_totals = sqlx::query_as::<_, (i64, i64)>(
                "SELECT COUNT(*), COALESCE(SUM(LENGTH(CAST(payload AS BLOB))), 0) FROM event_log",
            )
            .fetch_one(&self.read_pool)
            .await
            .ok();
            let blob_bytes =
                sqlx::query_scalar::<_, i64>("SELECT COALESCE(SUM(size), 0) FROM file_blob")
                    .fetch_one(&self.read_pool)
                    .await
                    .ok()
                    .and_then(|v| u64::try_from(v).ok());
            let events = event_totals
                .as_ref()
                .and_then(|(count, _)| u64::try_from(*count).ok());
            let bytes = event_totals
                .and_then(|(_, ev_bytes)| u64::try_from(ev_bytes).ok())
                .zip(blob_bytes)
                .map(|(e, b)| e.saturating_add(b));
            return ExitDrainReport::InMemoryFallback {
                path: info.path.clone(),
                events,
                bytes,
                reason: info.reason.clone(),
            };
        }
        if let Some(queue) = &self.write_queue {
            return queue.drain_for_exit(timeout).await;
        }
        ExitDrainReport::Clean
    }

    /// Drain pending appends up to `timeout` and write at most one durability
    /// status line to `out` per store instance when the run degraded, left
    /// unpersisted data, or ran in memory.
    pub async fn drain_and_report_exit_to(
        &self,
        timeout: Duration,
        mut out: impl std::io::Write,
    ) -> ExitDrainReport {
        let report = self.drain_for_exit(timeout).await;
        if let Some(line) = report.format_stderr_line()
            && !self
                .exit_reported
                .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            let _ = writeln!(out, "{line}");
        }
        report
    }

    /// Drain pending appends up to `timeout` and emit at most one durability
    /// status line to stderr when the run degraded, left unpersisted data, or
    /// ran in memory.
    pub async fn drain_and_report_exit(&self, timeout: Duration) -> ExitDrainReport {
        self.drain_and_report_exit_to(timeout, std::io::stderr())
            .await
    }

    /// Open an existing database read-only, without initialization or ownership.
    /// SQLite rejects a missing file; projection reads never persist snapshots.
    pub async fn connect_read_only(path: &str) -> Result<Self, StoreError> {
        let Some(paths) = database_paths(path)
            .map_err(|source| runtime_owner_lock_error(Some(Path::new(path)), source))?
        else {
            return Self::connect_memory().await;
        };
        let opts = SqliteConnectOptions::new()
            .filename(&paths.database)
            .read_only(true)
            .create_if_missing(false)
            .busy_timeout(FILE_POOL_WAIT_TIMEOUT);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .acquire_timeout(FILE_POOL_WAIT_TIMEOUT)
            // Closing the last connection of a WAL database checkpoints under an exclusive lock.
            .max_lifetime(None)
            .idle_timeout(None)
            .connect_with(opts)
            .await?;
        Ok(Self {
            writer_pool: pool.clone(),
            read_pool: pool,
            write_queue: None,
            read_timeout: READ_TIMEOUT,
            runtime_owner: Arc::new(RuntimeOwnerState::memory()),
            projections: Arc::new(ProjectionCache::new()),
            read_only: true,
            memory_fallback: None,
            exit_reported: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            _memory_keeper: None,
        })
    }

    /// Open an in-memory store (single connection) and run migrations.
    pub async fn connect_memory() -> Result<Self, StoreError> {
        // sqlx names each `:memory:` database uniquely in shared-cache mode,
        // so connections from these options all reach the same database.
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let keeper = sqlx::ConnectOptions::connect(&opts).await?;
        let pool = Self::writer_pool_options().connect_with(opts).await?;
        Self::migrate(&pool).await?;
        Ok(Self {
            writer_pool: pool.clone(),
            read_pool: pool,
            write_queue: None,
            read_timeout: READ_TIMEOUT,
            runtime_owner: Arc::new(RuntimeOwnerState::memory()),
            projections: Arc::new(ProjectionCache::new()),
            read_only: false,
            memory_fallback: None,
            exit_reported: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            _memory_keeper: Some(Arc::new(Mutex::new(keeper))),
        })
    }

    /// Set how many newly folded events make a projection read persist a
    /// durable snapshot (default 1024; `0` is treated as `1`, a snapshot on
    /// every read that folded new events). Applies to every clone of this store.
    ///
    /// Snapshots only change how much of a log a later read (or a restarted
    /// process) must fold; the folded result is the same either way.
    #[must_use]
    pub fn with_projection_snapshot_interval(self, events: u64) -> Self {
        self.projections.set_snapshot_interval(events);
        self
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

    async fn migrate(pool: &sqlx::SqlitePool) -> Result<(), StoreError> {
        // Serialize baseline inspection with initialization; SQLite sqlx migration locks are a no-op.
        let mut transaction = pool.begin_with("BEGIN IMMEDIATE").await?;
        let migrator = sqlx::migrate!("./migrations");
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&mut *transaction)
        .await?;
        if !tables.is_empty() {
            if !tables.iter().any(|name| name == "_sqlx_migrations") {
                return Err(StoreError::UnsupportedDatabase);
            }
            let checksum: Option<Vec<u8>> = sqlx::query_scalar(
                "SELECT checksum FROM _sqlx_migrations WHERE version = 1 AND success = 1",
            )
            .fetch_optional(&mut *transaction)
            .await?;
            if checksum.as_deref() != Some(migrator.migrations[0].checksum.as_ref()) {
                return Err(StoreError::UnsupportedDatabase);
            }
        }
        migrator.run_direct(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Begin a writer transaction for composing event appends with auxiliary rows.
    /// Publish appended envelopes only after committing the returned transaction.
    pub async fn begin_transaction(&self) -> Result<StoreTransaction<'_>, StoreError> {
        let (tx, guard) = if let Some(queue) = &self.write_queue {
            let (tx, guard) = queue.acquire_raw_transaction().await?;
            (tx, Some(guard))
        } else {
            (self.writer_pool.begin_with("BEGIN IMMEDIATE").await?, None)
        };
        Ok(StoreTransaction {
            tx,
            committed_events: Vec::new(),
            guard,
            _marker: std::marker::PhantomData,
        })
    }

    /// Append one domain event to the session log; returns the assigned sequence.
    pub async fn append_event(
        &self,
        session: SessionId,
        event: &Event,
    ) -> Result<(EventSeq, i64), StoreError> {
        let mut envelopes = self.append_events(session, vec![event.clone()]).await?;
        let envelope = envelopes
            .pop()
            .ok_or_else(|| StoreError::from(sqlx::Error::RowNotFound))?;
        Ok((envelope.seq, envelope.ts_millis))
    }

    /// Append an owned batch in order, returning envelopes only after commit.
    pub async fn append_events(
        &self,
        session: SessionId,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, StoreError> {
        if let Some(queue) = &self.write_queue {
            let batch = events.into_iter().map(|ev| (session, ev)).collect();
            return queue.submit(Vec::new(), batch).await;
        }
        let mut tx = self.begin_transaction().await?;
        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            envelopes.push(append_event_in_transaction(&mut tx, session, event).await?);
        }
        tx.commit().await?;
        Ok(envelopes)
    }

    /// Load all envelopes for a session in sequence order (payload JSON decoded to `Event`).
    pub async fn replay(&self, session: SessionId) -> Result<Vec<Envelope>, StoreError> {
        self.replay_page(session, 0, 0).await
    }

    /// Replay a root and all its descendants in durable global sequence order,
    /// including acknowledged writes still waiting for SQLite.
    pub async fn replay_tree(&self, root: SessionId) -> Result<Vec<Envelope>, StoreError> {
        // Capture the queue first: a concurrent flush can overlap the SELECT,
        // but cannot remove an envelope from both snapshots.
        let queued = self
            .write_queue
            .as_ref()
            .map(|q| q.acknowledged_batches())
            .unwrap_or_default();
        let (rows, mut known_sessions) = self
            .run_read_bounded("replay_tree", async {
                let mut read = self.read_pool_checked()?.begin().await?;
                let rows = sqlx::query(
                    "WITH RECURSIVE tree(id) AS (
                SELECT ? UNION SELECT s.id FROM session s JOIN tree t ON s.parent_id = t.id
            ) SELECT seq, session_id, ts, payload FROM event_log
              WHERE session_id IN (SELECT id FROM tree) ORDER BY seq",
                )
                .bind(root.storage_key())
                .fetch_all(&mut *read)
                .await?;
                let creates: std::collections::HashSet<_> = queued
                    .iter()
                    .flat_map(|batch| &batch.events)
                    .filter_map(|(_, envelope, _)| match envelope.event {
                        Event::SessionCreated { session, .. } => Some(session),
                        _ => None,
                    })
                    .collect();
                let mut known = std::collections::HashSet::new();
                // Read both tables at one snapshot even if the queue flushes meanwhile.
                let ids: Vec<_> = creates.into_iter().collect();
                for chunk in ids.chunks(500) {
                    let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
                        "SELECT id FROM session WHERE id IN (",
                    );
                    let mut values = query.separated(",");
                    for id in chunk {
                        values.push_bind(id.storage_key());
                    }
                    query.push(")");
                    for key in query
                        .build_query_scalar::<Vec<u8>>()
                        .fetch_all(&mut *read)
                        .await?
                    {
                        known.insert(decode_session_key(&key)?);
                    }
                }
                read.commit().await?;
                Ok((rows, known))
            })
            .await?;
        let mut sessions = std::collections::HashSet::from([root]);
        let mut envelopes = Vec::with_capacity(rows.len());
        for row in rows {
            let session = decode_session_key(row.try_get::<&[u8], _>("session_id")?)?;
            let seq: i64 = row.try_get("seq")?;
            sessions.insert(session);
            envelopes.push(Envelope {
                seq: EventSeq(seq.max(0) as u64),
                ts_millis: row.try_get("ts")?,
                event: decode_event(row.try_get("payload")?, session, seq)?,
            });
        }
        if !queued.is_empty() {
            // Queued creates are in submission order, with parents before
            // children (the session table enforces the parent foreign key).
            for batch in &queued {
                for (_, envelope, _) in &batch.events {
                    if let Event::SessionCreated {
                        session, parent, ..
                    } = &envelope.event
                        && known_sessions.insert(*session)
                        && parent.is_some_and(|id| sessions.contains(&id))
                    {
                        sessions.insert(*session);
                    }
                }
            }
            for batch in queued {
                envelopes.extend(
                    batch
                        .events
                        .iter()
                        .filter(|(session, _, _)| sessions.contains(session))
                        .map(|(_, envelope, _)| envelope.clone()),
                );
            }
            envelopes.sort_unstable_by_key(|env| env.seq);
            envelopes.dedup_by_key(|env| env.seq);
        }
        Ok(envelopes)
    }

    /// Read a sequence page without decoding the preceding event log.
    /// A zero limit requests the entire remaining tail.
    pub async fn replay_page(
        &self,
        session: SessionId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<Envelope>, StoreError> {
        let queued_tail = self
            .write_queue
            .as_ref()
            .map(|q| q.queued_envelopes_for(session, after));
        let query = sqlx::query(
            "SELECT seq, ts, payload FROM event_log WHERE session_id = ? AND seq > ? ORDER BY seq LIMIT ?",
        )
        .bind(session.storage_key())
        .bind(i64::try_from(after).unwrap_or(i64::MAX))
        .bind(if limit == 0 { -1 } else { i64::from(limit) });
        let rows = self
            .run_read_bounded("replay_page", async {
                query
                    .fetch_all(self.read_pool_checked()?)
                    .await
                    .map_err(StoreError::from)
            })
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let seq: i64 = row.try_get("seq")?;
            let payload: String = row.try_get("payload")?;
            out.push(Envelope {
                seq: EventSeq(seq.max(0) as u64),
                ts_millis: row.try_get("ts")?,
                event: decode_event(&payload, session, seq)?,
            });
        }
        if let Some(tail) = queued_tail {
            let disk_max = out.last().map_or(after, |env| env.seq.0);
            for env in tail.into_iter().filter(|env| env.seq.0 > disk_max) {
                if limit > 0 && out.len() >= limit as usize {
                    break;
                }
                out.push(env);
            }
        }
        Ok(out)
    }

    /// Return whether the event log contains a Session without replaying it.
    ///
    /// # Errors
    /// Returns SQLite failures.
    pub async fn session_exists(&self, session: SessionId) -> Result<bool, StoreError> {
        if self
            .write_queue
            .as_ref()
            .is_some_and(|queue| queue.has_queued_event(session))
        {
            return Ok(true);
        }
        let cached_exists = || {
            self.projections
                .get(session)
                .is_some_and(|base| base.projection.session.id.is_some())
        };
        if self
            .write_queue
            .as_ref()
            .is_some_and(write_queue::WriteQueue::is_degraded)
            && cached_exists()
        {
            return Ok(true);
        }
        let pool = match self.read_pool_checked() {
            Ok(pool) => pool,
            Err(StoreError::StoreDegraded { .. }) if cached_exists() => return Ok(true),
            Err(error) => return Err(error),
        };
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(SELECT 1 FROM event_log WHERE session_id = ? LIMIT 1)",
        )
        .bind(session.storage_key())
        .fetch_one(pool)
        .await?;
        Ok(exists != 0)
    }

    /// Delete all state owned by a session and every descendant in one
    /// transaction; returns whether the root itself had event rows.
    pub async fn delete_session(&self, session: SessionId) -> Result<bool, StoreError> {
        self.delete_session_where(session, None).await
    }

    /// Delete the session like [`SessionStore::delete_session`] (root and
    /// descendants), but only when its log still ends at `last_seq` (the
    /// newest sequence the caller checked). Returns `false`, deleting
    /// nothing, when the log grew since (or the session is gone). The check
    /// and the deletes share one write transaction, so no append can land
    /// between them.
    pub async fn delete_session_at(
        &self,
        session: SessionId,
        last_seq: EventSeq,
    ) -> Result<bool, StoreError> {
        self.delete_session_where(session, Some(last_seq)).await
    }

    async fn delete_session_where(
        &self,
        session: SessionId,
        last_seq: Option<EventSeq>,
    ) -> Result<bool, StoreError> {
        let key = session.storage_key();
        let mut tx = self.begin_transaction().await?;
        // The event log goes first: its conditional delete is the check (and
        // takes the write lock), so the cascade below only goes with it.
        let result = match last_seq {
            None => {
                sqlx::query("DELETE FROM event_log WHERE session_id = ?")
                    .bind(key.clone())
                    .execute(&mut *tx)
                    .await?
            }
            Some(last_seq) => {
                sqlx::query(
                    "DELETE FROM event_log WHERE session_id = ? \
                     AND (SELECT MAX(seq) FROM event_log WHERE session_id = ?) = ?",
                )
                .bind(key.clone())
                .bind(key.clone())
                .bind(i64::try_from(last_seq.0).unwrap_or(i64::MAX))
                .execute(&mut *tx)
                .await?
            }
        };
        let deleted = result.rows_affected() > 0;
        if !deleted && last_seq.is_some() {
            tx.rollback().await?;
            return Ok(false);
        }
        // The root and every descendant, from the materialized `session` tree
        // (`SessionCreated.parent` -> `session.parent_id`). The seed is the
        // bound key itself, so the tree resolves even when a `session` row is
        // missing, and the join on `parent_id` keeps unrelated sessions out;
        // `UNION` dedupes so a corrupt parent cycle cannot loop forever.
        let tree_keys = sqlx::query_scalar::<_, Vec<u8>>(
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT ? \
                 UNION \
                 SELECT s.id FROM session s JOIN tree t ON s.parent_id = t.id \
             ) \
             SELECT id FROM tree",
        )
        .bind(key)
        .fetch_all(&mut *tx)
        .await?;
        let sessions = tree_keys
            .iter()
            .map(|key| decode_session_key(key))
            .collect::<Result<Vec<_>, _>>()?;
        for tree_key in &tree_keys {
            for table in [
                "event_log",
                "open_assistant_message",
                "pending_resume",
                "projection_snapshot",
                "file_blob",
            ] {
                sqlx::query(&format!("DELETE FROM {table} WHERE session_id = ?"))
                    .bind(tree_key.as_slice())
                    .execute(&mut *tx)
                    .await?;
            }
        }
        for tree_key in &tree_keys {
            sqlx::query(
                "DELETE FROM resident_actor_claim WHERE actor_id = ? OR root_session_id = ?",
            )
            .bind(tree_key.as_slice())
            .bind(tree_key.as_slice())
            .execute(&mut *tx)
            .await?;
        }
        // Remove the complete tree in one statement to satisfy the self FK.
        sqlx::query(
            "WITH RECURSIVE tree(id) AS ( \
                 SELECT ? UNION SELECT s.id FROM session s JOIN tree t ON s.parent_id = t.id \
             ) DELETE FROM session WHERE id IN (SELECT id FROM tree)",
        )
        .bind(session.storage_key())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.projections.remove_all(sessions);
        Ok(deleted)
    }

    /// Fold the session event log into a [`Projection`] via the shared reducer.
    ///
    /// Served from the projection cache: only events after the cached fold are
    /// decoded and applied, and the result always equals
    /// `Projection::from_events(&self.replay(session).await?)`.
    pub async fn read_projection(&self, session: SessionId) -> Result<Projection, StoreError> {
        Ok(Projection::clone(&*self.cached_projection(session).await?))
    }

    /// Shared handle to the session's folded [`Projection`], without cloning it.
    ///
    /// Same fold and cache as [`SessionStore::read_projection`]; the handle is
    /// an immutable snapshot of the log as of this read.
    pub async fn read_projection_shared(
        &self,
        session: SessionId,
    ) -> Result<Arc<Projection>, StoreError> {
        self.cached_projection(session).await
    }

    /// Run `read` against the session's folded [`Projection`] without cloning it.
    ///
    /// Same fold and cache as [`SessionStore::read_projection`]; prefer this
    /// for hot paths that need a small part of a large projection.
    pub async fn with_projection<R>(
        &self,
        session: SessionId,
        read: impl FnOnce(&Projection) -> R,
    ) -> Result<R, StoreError> {
        Ok(read(&*self.cached_projection(session).await?))
    }

    /// Advance the cached fold of `session` before a writer transaction folds
    /// it, so the transaction reads only the tail.
    pub(crate) async fn warm_projection(&self, session: SessionId) -> Result<(), StoreError> {
        self.cached_projection(session).await.map(drop)
    }

    async fn load_prepared_projection(
        &self,
        session: SessionId,
        base_override: Option<projection_cache::Base>,
    ) -> Result<(projection_cache::PreparedFold, u64, u64), StoreError> {
        let mut conn = self.read_pool_checked()?.acquire().await?;
        let base = match base_override {
            Some(base) => Some(base),
            None => match self.projections.get(session) {
                Some(base) => Some(base),
                None => projection_cache::load_snapshot(&mut conn, session)
                    .await?
                    .map(|projection| projection_cache::Base {
                        persisted_seq: projection.last_seq,
                        unpersisted: 0,
                        projection: Arc::new(projection),
                    }),
            },
        };
        let (persisted_seq, unpersisted) = base
            .as_ref()
            .map_or((0, 0), |base| (base.persisted_seq, base.unpersisted));
        let prepared =
            projection_cache::fold(&mut conn, session, base.map(|base| base.projection)).await?;
        Ok((prepared, persisted_seq, unpersisted))
    }

    async fn run_bounded<F, T>(
        &self,
        operation: &'static str,
        bounded: bool,
        fut: F,
    ) -> Result<T, StoreError>
    where
        F: std::future::Future<Output = Result<T, StoreError>>,
    {
        match &self.write_queue {
            Some(queue) if bounded => {
                write_queue::run_with_timeout(operation, queue.write_timeout(), fut).await
            }
            _ => fut.await,
        }
    }

    pub(crate) fn read_pool_checked(&self) -> Result<&sqlx::SqlitePool, StoreError> {
        if let Some(queue) = &self.write_queue {
            queue.check_disk_readable()?;
        }
        Ok(&self.read_pool)
    }

    pub(crate) async fn run_read_bounded<F, T>(
        &self,
        operation: &'static str,
        fut: F,
    ) -> Result<T, StoreError>
    where
        F: std::future::Future<Output = Result<T, StoreError>>,
    {
        let degraded = self
            .write_queue
            .as_ref()
            .is_some_and(write_queue::WriteQueue::is_degraded);
        self.run_bounded(operation, degraded, fut).await
    }

    async fn fold_once_with_queue(
        &self,
        queue: &write_queue::WriteQueue,
        session: SessionId,
        base_override: Option<projection_cache::Base>,
        timeout: Option<Duration>,
    ) -> Result<write_queue::FoldOutcome, StoreError> {
        let snapshot = queue.begin_fold(session);
        let limit = timeout.or_else(|| snapshot.was_degraded.then(|| queue.write_timeout()));
        let load = self.load_prepared_projection(session, base_override);
        let (prepared, p, u) = match limit {
            Some(limit) => write_queue::run_with_timeout("read_projection", limit, load).await?,
            None => load.await?,
        };
        Ok(queue.finish_fold(session, snapshot, prepared, p, u))
    }

    /// Fold through the cache: cached base (in-process, else the durable
    /// snapshot) plus the events after it; persist a durable snapshot once
    /// enough events were folded since the last one.
    async fn cached_projection(&self, session: SessionId) -> Result<Arc<Projection>, StoreError> {
        let (projection, persisted_seq, unpersisted, allow_snapshot) = match &self.write_queue {
            None => {
                let (prepared, p, u) = self.load_prepared_projection(session, None).await?;
                let (folded, p, u) = self.projections.advance(session, prepared, p, u, &[]);
                (folded.projection, p, u, true)
            }
            Some(queue) => {
                if queue.is_degraded()
                    && let Some(base) = self.projections.get(session)
                {
                    return Ok(base.projection);
                }
                let outcome = match self.fold_once_with_queue(queue, session, None, None).await {
                    Ok(cached @ write_queue::FoldOutcome::Cached { .. }) => cached,
                    Ok(write_queue::FoldOutcome::Uncached {
                        folded,
                        persisted_seq,
                        unpersisted,
                    }) => {
                        if queue.is_degraded()
                            && let Some(base) = self.projections.get(session)
                        {
                            return Ok(base.projection);
                        }
                        let first_projection = Arc::clone(&folded.projection);
                        match self
                            .fold_once_with_queue(
                                queue,
                                session,
                                Some(projection_cache::Base {
                                    projection: folded.projection,
                                    persisted_seq,
                                    unpersisted,
                                }),
                                Some(self.read_timeout),
                            )
                            .await
                        {
                            Ok(outcome) => outcome,
                            Err(error) if error.is_storage_fault() => {
                                return Ok(first_projection);
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Err(error @ StoreError::StoreDegraded { .. }) => {
                        if let Some(base) = self.projections.get(session) {
                            return Ok(base.projection);
                        }
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                };
                match outcome {
                    write_queue::FoldOutcome::Cached {
                        folded,
                        persisted_seq,
                        unpersisted,
                        allow_snapshot,
                    } => (
                        folded.projection,
                        persisted_seq,
                        unpersisted,
                        allow_snapshot,
                    ),
                    write_queue::FoldOutcome::Uncached { folded, .. } => {
                        (folded.projection, 0, 0, false)
                    }
                }
            }
        };
        let last_seq = projection.last_seq;
        if allow_snapshot
            && !self.read_only
            && last_seq > persisted_seq
            && unpersisted >= self.projections.snapshot_interval()
        {
            match projection.encode_snapshot() {
                Ok(payload) => {
                    if let Some(mut wconn) = self.writer_pool.try_acquire() {
                        // Best effort: a busy writer or failed write only means a later read folds more.
                        let persist_res = self
                            .run_bounded(
                                "persist_snapshot",
                                true,
                                projection_cache::persist_snapshot(
                                    &mut wconn, session, last_seq, payload,
                                ),
                            )
                            .await;
                        match persist_res {
                            Ok(()) => {
                                self.projections.mark_persisted(session, last_seq);
                            }
                            Err(error) => {
                                tracing::debug!(
                                    %session,
                                    "projection snapshot not persisted: {error}"
                                );
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::debug!(%session, "projection snapshot not encoded: {error}");
                }
            }
        }
        Ok(projection)
    }

    /// Sessions present in the event log, newest-updated first.
    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError> {
        let rows = sqlx::query(
            "SELECT session_id, MIN(ts) AS started, MAX(ts) AS updated, COUNT(*) AS n \
             FROM event_log GROUP BY session_id ORDER BY updated DESC, session_id DESC",
        )
        .fetch_all(self.read_pool_checked()?)
        .await?;
        session_infos(rows)
    }

    /// [`SessionStore::list_sessions`], optionally narrowed to the sessions
    /// (root and subagent) whose `session_created` named `project`
    /// (ADR-0024). `None` lists every session.
    pub async fn list_sessions_in(
        &self,
        project: Option<ProjectId>,
    ) -> Result<Vec<SessionInfo>, StoreError> {
        let Some(project) = project else {
            return self.list_sessions().await;
        };
        let rows = sqlx::query(
            "SELECT e.session_id, MIN(e.ts) AS started, MAX(e.ts) AS updated, COUNT(*) AS n \
             FROM event_log e JOIN session s ON s.id = e.session_id \
             WHERE s.project_id = ? \
             GROUP BY e.session_id ORDER BY updated DESC, e.session_id DESC",
        )
        .bind(project.to_string())
        .fetch_all(self.read_pool_checked()?)
        .await?;
        session_infos(rows)
    }

    /// Sessions of one Project whose latest event is at or after
    /// `since_millis`, newest first, at most `limit` (clamped to 1..=200),
    /// optionally leaving out `exclude`.
    pub async fn list_sessions_in_since(
        &self,
        project: ProjectId,
        since_millis: i64,
        limit: usize,
        exclude: Option<SessionId>,
    ) -> Result<Vec<SessionInfo>, StoreError> {
        let limit = i64::try_from(limit.clamp(1, 200)).unwrap_or(200);
        let rows = sqlx::query(
            "SELECT e.session_id, MIN(e.ts) AS started, MAX(e.ts) AS updated, COUNT(*) AS n \
             FROM event_log e JOIN session s ON s.id = e.session_id \
             WHERE s.project_id = ? AND (? IS NULL OR e.session_id != ?) \
             GROUP BY e.session_id HAVING MAX(e.ts) >= ? \
             ORDER BY updated DESC, e.session_id DESC LIMIT ?",
        )
        .bind(project.to_string())
        .bind(exclude.map(|s| s.storage_key()))
        .bind(exclude.map(|s| s.storage_key()))
        .bind(since_millis)
        .bind(limit)
        .fetch_all(self.read_pool_checked()?)
        .await?;
        session_infos(rows)
    }

    /// Most recent file changes made by sessions of one Project since
    /// `since_millis`, newest first, one row per path (the last writer among
    /// the considered sessions wins), at most `limit` rows (clamped to
    /// 1..=200). `exclude` leaves one session's changes out before picking the
    /// last writer.
    ///
    /// One bounded query over `files_changed` rows: no projection replay. The
    /// change time is the event's append timestamp; `created` is true when the
    /// path did not exist before that change.
    pub async fn project_activity_files(
        &self,
        project: ProjectId,
        since_millis: i64,
        limit: usize,
        exclude: Option<SessionId>,
    ) -> Result<Vec<ProjectActivityFile>, StoreError> {
        let limit = i64::try_from(limit.clamp(1, 200)).unwrap_or(200);
        let rows = sqlx::query(
            "SELECT session_id, ts, path, before_kind FROM ( \
               SELECT e.session_id AS session_id, e.ts AS ts, \
                      json_extract(f.value, '$.path') AS path, \
                      json_extract(f.value, '$.before.kind') AS before_kind, \
                      ROW_NUMBER() OVER ( \
                        PARTITION BY json_extract(f.value, '$.path') \
                        ORDER BY e.ts DESC, e.seq DESC \
                      ) AS rn \
               FROM event_log e \
               JOIN session s ON s.id = e.session_id, \
                    json_each(e.payload, '$.files') f \
               WHERE s.project_id = ? AND e.ts >= ? \
                 AND (? IS NULL OR e.session_id != ?) \
                 AND json_extract(e.payload, '$.type') = 'files_changed' \
             ) WHERE rn = 1 AND path IS NOT NULL \
             ORDER BY ts DESC, path ASC LIMIT ?",
        )
        .bind(project.to_string())
        .bind(since_millis)
        .bind(exclude.map(|s| s.storage_key()))
        .bind(exclude.map(|s| s.storage_key()))
        .bind(limit)
        .fetch_all(self.read_pool_checked()?)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let key: Vec<u8> = row.try_get("session_id")?;
            let session = decode_session_key(&key)?;
            let before_kind: Option<String> = row.try_get("before_kind")?;
            out.push(ProjectActivityFile {
                path: row.try_get("path")?,
                session,
                changed_millis: row.try_get("ts")?,
                created: before_kind.as_deref() == Some("absent"),
            });
        }
        Ok(out)
    }

    /// One session's [`SessionInfo`] (log bounds and event count), without
    /// grouping the whole log; `None` when the session has no events.
    pub async fn session_info(
        &self,
        session: SessionId,
    ) -> Result<Option<SessionInfo>, StoreError> {
        let row = sqlx::query(
            "SELECT MIN(ts) AS started, MAX(ts) AS updated, COUNT(*) AS n \
             FROM event_log WHERE session_id = ?",
        )
        .bind(session.storage_key())
        .fetch_one(self.read_pool_checked()?)
        .await?;
        let n: i64 = row.try_get("n")?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(SessionInfo {
            session,
            started_millis: row.try_get("started")?,
            updated_millis: row.try_get("updated")?,
            events: n.max(0) as u64,
        }))
    }
}

pub(crate) fn validate_event_for_append(event: &Event) -> Result<(), StoreError> {
    if let Event::MailSent {
        from,
        to: hya_proto::MailEndpoint::Handle(handle),
        ..
    } = event
        && from != hya_proto::scope::HARNESS_HANDLE
    {
        return Err(StoreError::MailAuthorizationRejected {
            sender: from.clone(),
            channel: format!("direct:{handle}"),
            reason: "agent mail must target a channel".into(),
        });
    }
    Ok(())
}

pub(crate) async fn insert_event_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session: SessionId,
    event: &Event,
    fixed: Option<(u64, i64, &str)>,
) -> Result<(EventSeq, i64), StoreError> {
    let owned_payload;
    let (expected, ts_millis, payload) = match fixed {
        Some((seq, ts, payload)) => {
            let exp = i64::try_from(seq).map_err(|_| {
                StoreError::from(sqlx::Error::Protocol("sequence exceeds i64 range".into()))
            })?;
            (Some(exp), ts, payload)
        }
        None => {
            validate_event_for_append(event)?;
            owned_payload = serde_json::to_string(event)?;
            (None, now_millis(), owned_payload.as_str())
        }
    };
    let seq: i64 = sqlx::query_scalar(
        "INSERT INTO event_log (seq, session_id, payload, ts) VALUES (?, ?, ?, ?) RETURNING seq",
    )
    .bind(expected)
    .bind(session.storage_key())
    .bind(payload)
    .bind(ts_millis)
    .fetch_one(&mut **tx)
    .await?;
    materialize::materialize_event_side_tables(tx, session, event, ts_millis).await?;
    match event {
        Event::MessageStarted {
            role: hya_proto::Role::Assistant,
            ..
        } => {
            sqlx::query("DELETE FROM pending_resume WHERE session_id = ?")
                .bind(session.storage_key())
                .execute(&mut **tx)
                .await?;
        }
        Event::MessageFinished {
            message,
            role: hya_proto::Role::Assistant,
            finish: hya_proto::FinishReason::Cancelled,
            cause: Some(hya_proto::FinishCause::Shutdown),
            ..
        } => {
            let base = projection_cache::load_snapshot(tx, session)
                .await?
                .map(Arc::new);
            let proj = projection_cache::fold(tx, session, base)
                .await?
                .finish()
                .projection;
            if proj.session.id.is_some()
                && proj.session.parent.is_none()
                && !proj.session.is_archived()
            {
                sqlx::query(
                    "INSERT INTO pending_resume (session_id, message_id, created_at) VALUES (?, ?, ?) \
                     ON CONFLICT(session_id) DO UPDATE SET message_id = excluded.message_id, \
                     prompt_message_id = NULL, created_at = excluded.created_at",
                )
                .bind(session.storage_key())
                .bind(message.to_string())
                .bind(ts_millis)
                .execute(&mut **tx)
                .await?;
            }
        }
        _ => {}
    }
    Ok((EventSeq(seq.max(0) as u64), ts_millis))
}

/// Active writer database transaction synchronized with the append queue.
pub struct StoreTransaction<'a> {
    tx: sqlx::Transaction<'static, sqlx::Sqlite>,
    committed_events: Vec<(SessionId, Envelope)>,
    guard: Option<write_queue::RawTxGuard>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl std::fmt::Debug for StoreTransaction<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreTransaction")
            .field("committed_events", &self.committed_events.len())
            .field("queued", &self.guard.is_some())
            .finish()
    }
}

impl std::ops::Deref for StoreTransaction<'_> {
    type Target = sqlx::SqliteConnection;

    fn deref(&self) -> &Self::Target {
        &self.tx
    }
}

impl std::ops::DerefMut for StoreTransaction<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tx
    }
}

impl<'a> StoreTransaction<'a> {
    pub(crate) fn inner_mut(&mut self) -> &mut sqlx::Transaction<'static, sqlx::Sqlite> {
        &mut self.tx
    }

    pub(crate) fn record_event(&mut self, session: SessionId, envelope: Envelope) {
        if let Some(guard) = &mut self.guard {
            guard.record_event(session, envelope.seq.0);
        }
        self.committed_events.push((session, envelope));
    }

    /// Commit the transaction and publish its appended events into the queue and warm cache.
    ///
    /// Resolves `tx.commit()` and releases the raw-transaction guard inside a spawned task so
    /// cancelling the caller's future never releases the guard while SQLite COMMIT is in flight.
    pub async fn commit(self) -> Result<(), StoreError> {
        let Self {
            tx,
            committed_events,
            mut guard,
            ..
        } = self;
        let Some(guard) = guard.take() else {
            tx.commit().await?;
            return Ok(());
        };
        let handle = tokio::spawn(async move {
            match tx.commit().await {
                Ok(()) => {
                    guard.finish_commit(&committed_events);
                    Ok(())
                }
                Err(error) => {
                    drop(guard);
                    Err(StoreError::from(error))
                }
            }
        });
        handle.await.map_err(|err| {
            StoreError::from(sqlx::Error::Protocol(format!("commit task failed: {err}")))
        })?
    }

    /// Roll back the transaction and release the append-queue barrier without updating cache state.
    pub async fn rollback(mut self) -> Result<(), StoreError> {
        self.tx.rollback().await?;
        drop(self.guard.take());
        Ok(())
    }
}

/// Append one event inside an existing writer transaction.
pub async fn append_event_in_transaction(
    tx: &mut StoreTransaction<'_>,
    session: SessionId,
    event: Event,
) -> Result<Envelope, StoreError> {
    let (seq, ts_millis) = insert_event_in_tx(tx.inner_mut(), session, &event, None).await?;
    let envelope = Envelope {
        seq,
        ts_millis,
        event,
    };
    tx.record_event(session, envelope.clone());
    Ok(envelope)
}

/// Fold one Session log inside an existing writer transaction.
///
/// Starts from the cached fold (anchor-checked through the transaction) and
/// sees the transaction's own uncommitted events, so the result is never
/// written back to the cache.
pub(crate) async fn replay_projection(
    cache: &ProjectionCache,
    tx: &mut StoreTransaction<'_>,
    session: SessionId,
) -> Result<Arc<Projection>, StoreError> {
    let base = projection_cache::transaction_base(cache, tx, session).await?;
    let folded = projection_cache::fold(tx, session, base).await?.finish();
    Ok(folded.projection)
}

/// Decode `list_sessions`-shaped rows (`session_id`, `started`, `updated`,
/// `n`), rejecting malformed session keys.
fn session_infos(rows: Vec<sqlx::sqlite::SqliteRow>) -> Result<Vec<SessionInfo>, StoreError> {
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let key: Vec<u8> = r.try_get("session_id")?;
        let started: i64 = r.try_get("started")?;
        let updated: i64 = r.try_get("updated")?;
        let n: i64 = r.try_get("n")?;
        let session = decode_session_key(&key)?;
        out.push(SessionInfo {
            session,
            started_millis: started,
            updated_millis: updated,
            events: n.max(0) as u64,
        });
    }
    Ok(out)
}

pub(crate) fn decode_session_key(key: &[u8]) -> Result<SessionId, StoreError> {
    let raw = std::str::from_utf8(key).map_err(|error| {
        StoreError::SessionKeyData(format!("invalid UTF-8 key {key:?}: {error}"))
    })?;
    raw.parse()
        .map_err(|error| StoreError::SessionKeyData(format!("invalid key {raw:?}: {error}")))
}

pub(crate) fn decode_event(
    payload: &str,
    session: SessionId,
    seq: i64,
) -> Result<Event, StoreError> {
    serde_json::from_str(payload).map_err(|source| StoreError::EventDecode {
        session,
        seq,
        source: Arc::new(source),
    })
}

/// Serialize unit tests in this binary that open file-backed `SessionStore`s
/// so `lifecycle-bench`'s process-wide SQLite trace counters observe only the
/// benchmark test's own statements.
#[cfg(test)]
pub(crate) async fn file_store_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod event_contract_tests {
    use super::*;

    #[tokio::test]
    async fn memory_spellings_keep_data_across_pool_connections() {
        for path in ["", ":memory:"] {
            for store in [
                SessionStore::connect(path).await.unwrap(),
                SessionStore::connect_runtime(path).await.unwrap(),
                SessionStore::connect_read_only(path).await.unwrap(),
            ] {
                let session = SessionId::new();
                store
                    .append_event(
                        session,
                        &Event::SessionArchived {
                            session,
                            archived: 1.into(),
                        },
                    )
                    .await
                    .unwrap();
                for _ in 0..3 {
                    store
                        .writer_pool
                        .acquire()
                        .await
                        .unwrap()
                        .close()
                        .await
                        .unwrap();
                    assert_eq!(store.replay(session).await.unwrap().len(), 1);
                }
            }
        }
    }

    #[tokio::test]
    async fn tree_replay_merges_persisted_and_queued_descendants()
    -> Result<(), Box<dyn std::error::Error>> {
        let _guard = file_store_test_lock().await;
        let dir = std::env::temp_dir().join(format!("hya-tree-{}", SessionId::new()));
        std::fs::create_dir(&dir)?;
        let db = dir.join("tree.db");
        let store = SessionStore::connect_with_timeouts(
            db.to_str().ok_or("invalid db path")?,
            Duration::from_millis(100),
            Duration::from_secs(2),
        )
        .await?;
        let (root, child, grandchild, other) = (
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
        );
        let created = |session, parent| Event::SessionCreated {
            session,
            parent,
            agent: "build".into(),
            model: "fake".into(),
            workdir: ".".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        };
        store.append_event(root, &created(root, None)).await?;
        store.append_event(other, &created(other, None)).await?;
        let held = store.writer_pool.begin_with("BEGIN IMMEDIATE").await?;
        let queued_root = SessionId::new();
        store
            .append_event(queued_root, &created(queued_root, None))
            .await?;
        for (session, parent) in [
            (child, root),
            (grandchild, child),
            (other, root),
            (queued_root, child),
        ] {
            store
                .append_event(session, &created(session, Some(parent)))
                .await?;
        }
        store
            .append_event(
                other,
                &Event::SessionTitled {
                    session: other,
                    title: "elsewhere".into(),
                },
            )
            .await?;
        assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
        let queued = store.replay_tree(root).await?;
        assert_eq!(
            queued
                .iter()
                .filter_map(|env| env.event.session())
                .collect::<Vec<_>>(),
            vec![root, child, grandchild]
        );
        assert!(queued.windows(2).all(|pair| pair[0].seq < pair[1].seq));
        held.rollback().await?;
        let report = store.drain_for_exit(Duration::from_secs(5)).await;
        assert!(matches!(report, ExitDrainReport::Recovered { .. }));
        assert_eq!(
            serde_json::to_value(store.replay_tree(root).await?)?,
            serde_json::to_value(queued)?
        );
        drop(store);
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn replay_page_decodes_only_the_sql_interval() {
        let store = SessionStore::connect_memory().await.unwrap();
        let session = SessionId::new();
        let event = Event::SessionCreated {
            session,
            parent: None,
            agent: "build".into(),
            model: "fake".into(),
            workdir: "/tmp".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        };
        for _ in 0..5 {
            store.append_event(session, &event).await.unwrap();
        }
        let all = store.replay(session).await.unwrap();
        for (offset, limit) in [(0, 2), (2, 2), (4, 2), (5, 2), (2, 0)] {
            let since = if offset == 0 {
                0
            } else {
                all[offset - 1].seq.0
            };
            let page = store.replay_page(session, since, limit).await.unwrap();
            let expected: Vec<_> = all
                .iter()
                .skip(offset)
                .take(if limit == 0 {
                    usize::MAX
                } else {
                    limit as usize
                })
                .collect();
            assert_eq!(
                serde_json::to_value(page).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
        }
        sqlx::query("UPDATE event_log SET payload = 'invalid' WHERE seq = ?")
            .bind(all[0].seq.0 as i64)
            .execute(&store.writer_pool)
            .await
            .unwrap();
        assert_eq!(
            store
                .replay_page(session, all[0].seq.0, 1)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .replay_page(session, u64::MAX, 1)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn deletion_removes_claims_owned_by_the_root_even_without_actor_sessions() {
        let store = SessionStore::connect_memory().await.unwrap();
        let root = SessionId::new();
        let other = SessionId::new();
        for session in [root, other] {
            store
                .append_event(
                    session,
                    &Event::SessionCreated {
                        session,
                        parent: None,
                        agent: "build".into(),
                        model: "fake".into(),
                        workdir: "/tmp".into(),
                        project: None,
                        kind: hya_proto::SessionKind::Project,
                    },
                )
                .await
                .unwrap();
        }
        let missing_actor = SessionId::new();
        let unrelated_actor = SessionId::new();
        for (actor, owner) in [(missing_actor, root), (unrelated_actor, other)] {
            sqlx::query("INSERT INTO resident_actor_claim (actor_id, root_session_id, epoch, owner_run_id, state) VALUES (?, ?, 1, randomblob(16), 'active')")
                .bind(actor.storage_key()).bind(owner.storage_key()).execute(&store.writer_pool).await.unwrap();
        }
        let seq = store.replay(root).await.unwrap().last().unwrap().seq;
        assert!(
            !store
                .delete_session_at(root, EventSeq(seq.0 + 1))
                .await
                .unwrap()
        );
        assert_eq!(store.live_member_count(root).await.unwrap(), 1);
        assert!(store.delete_session_at(root, seq).await.unwrap());
        assert_eq!(store.live_member_count(root).await.unwrap(), 0);
        assert_eq!(store.live_member_count(other).await.unwrap(), 1);
        assert!(store.replay(other).await.is_ok());
    }

    #[tokio::test]
    async fn deletion_removes_every_session_owned_table() {
        let store = SessionStore::connect_memory().await.unwrap();
        let root = SessionId::new();
        let child = SessionId::new();
        let other = SessionId::new();
        for (session, parent) in [(root, None), (child, Some(root)), (other, None)] {
            store
                .append_event(
                    session,
                    &Event::SessionCreated {
                        session,
                        parent,
                        agent: "build".into(),
                        model: "fake".into(),
                        workdir: "/tmp".into(),
                        project: None,
                        kind: hya_proto::SessionKind::Project,
                    },
                )
                .await
                .unwrap();
            store
                .append_event(
                    session,
                    &Event::MessageStarted {
                        session,
                        message: hya_proto::MessageId::new(),
                        role: hya_proto::Role::Assistant,
                        agent: None,
                        model: None,
                    },
                )
                .await
                .unwrap();
            store.read_projection(session).await.unwrap();
            store.put_file_blob(session, "hash", &[1]).await.unwrap();
            for sql in [
                "INSERT INTO projection_snapshot VALUES (?, 1, 1, X'01')",
                "INSERT INTO pending_resume VALUES (?, 'message', NULL, 1)",
            ] {
                sqlx::query(sql)
                    .bind(session.storage_key())
                    .execute(&store.writer_pool)
                    .await
                    .unwrap();
            }
            sqlx::query("INSERT INTO resident_actor_claim (actor_id, root_session_id, epoch, owner_run_id, state) VALUES (?, ?, 1, randomblob(16), 'active')")
                .bind(session.storage_key())
                .bind(parent.unwrap_or(session).storage_key())
                .execute(&store.writer_pool).await.unwrap();
        }
        for sql in [
            "INSERT INTO project VALUES ('project', 'Project', 1, 1, 0)",
            "INSERT INTO project_root VALUES ('project', '/tmp', 0)",
            "INSERT INTO saved_permission VALUES ('grant', 'project', 'read', '*', 1)",
            "INSERT INTO saved_permission VALUES ('global-grant', 'global', 'read', '*', 1)",
            "INSERT INTO agent_model_preference VALUES ('build', 'fake', 'fake')",
            "INSERT INTO model_effort_preference VALUES ('fake', 'fake', 'high', 1)",
            "INSERT INTO agent_effort_preference VALUES ('build', 'high', 1)",
        ] {
            sqlx::query(sql).execute(&store.writer_pool).await.unwrap();
        }
        // A late failure must roll back the event and auxiliary deletes too.
        sqlx::query("CREATE TRIGGER reject_delete BEFORE DELETE ON session BEGIN SELECT RAISE(ABORT, 'delete blocked'); END")
            .execute(&store.writer_pool).await.unwrap();
        assert!(store.delete_session(root).await.is_err());
        assert_eq!(store.replay(root).await.unwrap().len(), 2);
        assert!(store.file_blob(child, "hash").await.unwrap().is_some());
        sqlx::query("DROP TRIGGER reject_delete")
            .execute(&store.writer_pool)
            .await
            .unwrap();
        assert!(store.delete_session(root).await.unwrap());
        for (table, column) in [
            ("session", "id"),
            ("event_log", "session_id"),
            ("open_assistant_message", "session_id"),
            ("pending_resume", "session_id"),
            ("projection_snapshot", "session_id"),
            ("file_blob", "session_id"),
            ("resident_actor_claim", "actor_id"),
        ] {
            for session in [root, child] {
                let count: i64 =
                    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?"))
                        .bind(session.storage_key())
                        .fetch_one(&store.read_pool)
                        .await
                        .unwrap();
                assert_eq!(count, 0, "{table} retains {session}");
            }
            let count: i64 =
                sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?"))
                    .bind(other.storage_key())
                    .fetch_one(&store.read_pool)
                    .await
                    .unwrap();
            assert!(count > 0, "{table} lost unrelated state");
        }
        for table in [
            "project",
            "project_root",
            "saved_permission",
            "agent_model_preference",
            "model_effort_preference",
            "agent_effort_preference",
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&store.read_pool)
                .await
                .unwrap();
            assert_eq!(
                count,
                if table == "saved_permission" { 2 } else { 1 },
                "{table}"
            );
        }
    }

    #[tokio::test]
    async fn unknown_event_tags_fail_replay_with_row_context() {
        let store = SessionStore::connect_memory().await.unwrap();
        let session = SessionId::new();
        let seq = sqlx::query("INSERT INTO event_log (session_id, ts, payload) VALUES (?, 1, ?)")
            .bind(session.storage_key())
            .bind(format!(
                r#"{{"type":"future_event","session":"{session}"}}"#
            ))
            .execute(&store.writer_pool)
            .await
            .unwrap()
            .last_insert_rowid();

        for error in [
            store.replay(session).await.unwrap_err(),
            store.read_projection(session).await.unwrap_err(),
        ] {
            assert!(
                matches!(error, StoreError::EventDecode { session: owner, seq: row, .. } if owner == session && row == seq)
            );
            let text = error.to_string();
            assert!(text.contains("future_event"), "{text}");
            assert!(text.contains(&session.to_string()), "{text}");
            assert!(text.contains(&format!("sequence {seq}")), "{text}");
        }
    }

    #[tokio::test]
    async fn invalid_session_keys_fail_listing_instead_of_disappearing() {
        let store = SessionStore::connect_memory().await.unwrap();
        // This legacy binary UUID is valid UTF-8, but is not a hysec text key.
        let key = uuid::Uuid::parse_str("41414141-4141-40c2-8041-414141414141")
            .unwrap()
            .as_bytes()
            .to_vec();
        sqlx::query("INSERT INTO event_log (session_id, ts, payload) VALUES (?, 1, '{}')")
            .bind(key)
            .execute(&store.writer_pool)
            .await
            .unwrap();
        let error = store.list_sessions().await.unwrap_err();
        assert!(matches!(error, StoreError::SessionKeyData(_)), "{error}");
        assert!(error.to_string().contains("hysec_"), "{error}");
        assert!(decode_session_key(&[0xff; 16]).is_err());
    }

    #[tokio::test]
    async fn sqlite_pools_disable_lifetime_and_idle_retirement() {
        let _guard = file_store_test_lock().await;
        let dir = std::env::temp_dir().join(format!(
            "hya-store-pool-opts-{}-{}",
            std::process::id(),
            now_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("sessions.db");
        let rw = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        assert_eq!(rw.writer_pool.options().get_max_connections(), 1);
        assert!(!rw.writer_pool.options().get_test_before_acquire());
        assert_eq!(rw.writer_pool.options().get_max_lifetime(), None);
        assert_eq!(rw.writer_pool.options().get_idle_timeout(), None);
        assert_eq!(rw.read_pool.options().get_max_lifetime(), None);
        assert_eq!(rw.read_pool.options().get_idle_timeout(), None);

        let ro = SessionStore::connect_read_only(db.to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(ro.read_pool.options().get_max_lifetime(), None);
        assert_eq!(ro.read_pool.options().get_idle_timeout(), None);

        let mem = SessionStore::connect_memory().await.unwrap();
        assert!(!mem.writer_pool.options().get_test_before_acquire());
        assert_eq!(mem.writer_pool.options().get_max_lifetime(), None);
        assert_eq!(mem.writer_pool.options().get_idle_timeout(), None);

        ro.read_pool.close().await;
        rw.writer_pool.close().await;
        rw.read_pool.close().await;
        drop(ro);
        drop(rw);
        drop(mem);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn snapshot_persist_skips_without_waiting_when_writer_is_busy() {
        let _guard = file_store_test_lock().await;
        let dir = std::env::temp_dir().join(format!(
            "hya-store-snapshot-try-{}-{}",
            std::process::id(),
            now_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("sessions.db");
        let store = SessionStore::connect(db.to_str().unwrap())
            .await
            .unwrap()
            .with_projection_snapshot_interval(1);
        let session = SessionId::new();
        store
            .append_event(
                session,
                &Event::SessionCreated {
                    session,
                    parent: None,
                    agent: hya_proto::AgentName::new("build"),
                    model: hya_proto::ModelRef::new("fake"),
                    workdir: "/tmp".into(),
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                },
            )
            .await
            .unwrap();

        let tx = store.begin_transaction().await.unwrap();
        let started = std::time::Instant::now();
        for _ in 0..3 {
            let proj = store.read_projection(session).await.unwrap();
            assert_eq!(proj.last_seq, 1);
        }
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "busy writer must not delay snapshot reads (elapsed={:?})",
            started.elapsed()
        );
        let count_busy: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projection_snapshot")
            .fetch_one(&store.read_pool)
            .await
            .unwrap();
        assert_eq!(count_busy, 0);

        drop(tx);
        tokio::time::timeout(Duration::from_secs(2), async {
            while store.writer_pool.num_idle() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("writer connection returns to idle");

        let proj = store.read_projection(session).await.unwrap();
        assert_eq!(proj.last_seq, 1);
        let count_idle: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projection_snapshot")
            .fetch_one(&store.read_pool)
            .await
            .unwrap();
        assert_eq!(count_idle, 1);

        store.writer_pool.close().await;
        store.read_pool.close().await;
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn writer_and_read_pools_are_strictly_separated() {
        let _guard = file_store_test_lock().await;
        let dir = std::env::temp_dir().join(format!(
            "hya-store-pool-split-{}-{}",
            std::process::id(),
            now_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("sessions.db");
        let store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        assert_eq!(store.writer_pool.options().get_max_connections(), 1);
        assert_eq!(store.read_pool.options().get_max_connections(), 4);
        assert!(!store.writer_pool.options().get_test_before_acquire());

        let session = SessionId::new();
        store
            .append_event(
                session,
                &Event::SessionCreated {
                    session,
                    parent: None,
                    agent: hya_proto::AgentName::new("build"),
                    model: hya_proto::ModelRef::new("fake"),
                    workdir: "/tmp".into(),
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            store.read_pool.size(),
            0,
            "writes must never touch the lazy read pool"
        );
        assert_eq!(store.writer_pool.size(), 1);

        let tx = store.begin_transaction().await.unwrap();
        assert!(
            store.writer_pool.try_acquire().is_none(),
            "sole writer connection must be occupied while tx is held"
        );
        let deadline = Duration::from_millis(500);
        assert_eq!(
            tokio::time::timeout(deadline, store.replay(session))
                .await
                .unwrap()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            tokio::time::timeout(deadline, store.list_sessions())
                .await
                .unwrap()
                .unwrap()
                .len(),
            1
        );
        assert!(
            tokio::time::timeout(deadline, store.session_exists(session))
                .await
                .unwrap()
                .unwrap()
        );
        assert_eq!(
            tokio::time::timeout(deadline, store.read_projection(session))
                .await
                .unwrap()
                .unwrap()
                .last_seq,
            1
        );
        assert!(
            store.read_pool.size() >= 1,
            "reads must execute on read_pool"
        );
        let ro_write_err =
            sqlx::query("INSERT INTO event_log (session_id, ts, payload) VALUES (?, 1, '{}')")
                .bind(session.storage_key())
                .execute(&store.read_pool)
                .await
                .unwrap_err();
        assert!(
            ro_write_err.to_string().contains("readonly"),
            "read_pool must reject writes: {ro_write_err}"
        );

        tx.rollback().await.unwrap();
        store.writer_pool.close().await;
        store.read_pool.close().await;
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn migration_script_storage_fault_is_classified() {
        let io = sqlx::Error::Io(std::io::Error::other("injected EIO"));
        let err = StoreError::from(sqlx::migrate::MigrateError::ExecuteMigration(io, 1));
        assert!(is_open_storage_fault(&err), "{err}");
    }

    #[test]
    fn commit_task_panic_maps_to_non_storage_error() {
        let err = StoreError::from(sqlx::Error::Protocol(
            "commit task failed: task panicked".into(),
        ));
        assert!(!err.is_storage_fault(), "{err}");
    }

    #[test]
    fn is_open_storage_fault_classifies_migration_arms() {
        let io_exec = StoreError::from(sqlx::migrate::MigrateError::Execute(sqlx::Error::Io(
            std::io::Error::other("injected EIO"),
        )));
        let io_script = StoreError::from(sqlx::migrate::MigrateError::ExecuteMigration(
            sqlx::Error::Io(std::io::Error::other("injected script EIO")),
            1,
        ));
        let non_storage_exec = StoreError::from(sqlx::migrate::MigrateError::Execute(
            sqlx::Error::Protocol("bad migration SQL".into()),
        ));
        let non_storage_script = StoreError::from(sqlx::migrate::MigrateError::ExecuteMigration(
            sqlx::Error::Protocol("bad script SQL".into()),
            1,
        ));
        let missing = StoreError::from(sqlx::migrate::MigrateError::VersionMissing(1));

        assert!(is_open_storage_fault(&io_exec), "{io_exec}");
        assert!(is_open_storage_fault(&io_script), "{io_script}");
        assert!(!is_open_storage_fault(&non_storage_exec));
        assert!(!is_open_storage_fault(&non_storage_script));
        assert!(!is_open_storage_fault(&missing));
    }

    #[test]
    fn validate_database_arg_is_lexical_and_rejects_uris() {
        assert!(validate_database_arg("").is_ok());
        assert!(validate_database_arg(":memory:").is_ok());
        assert!(validate_database_arg("/nonexistent/unlikely/path/hya.db").is_ok());
        assert_eq!(
            validate_database_arg("file:hya.db").unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            validate_database_arg("sqlite:hya.db").unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn stalled_filesystem_open_times_out_and_exits_runtime_promptly() {
        if std::env::var_os("HYA_TEST_STALLED_OPEN_CHILD").is_none() {
            let started = std::time::Instant::now();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "event_contract_tests::stalled_filesystem_open_times_out_and_exits_runtime_promptly",
                    "--nocapture",
                ])
                .env("HYA_TEST_STALLED_OPEN_CHILD", "1")
                .output()
                .unwrap();
            let elapsed = started.elapsed();
            assert!(
                output.status.success(),
                "stalled open child failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                elapsed < Duration::from_secs(4),
                "process exit hung waiting on stalled open thread ({elapsed:?})"
            );
            return;
        }

        TEST_STALL_OPEN.store(true, std::sync::atomic::Ordering::Release);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let bound = Duration::from_millis(120);
        rt.block_on(async {
            let strict_res = SessionStore::connect_runtime_bounded(
                "/tmp/stalled-strict.db",
                bound,
                WRITE_TIMEOUT,
                READ_TIMEOUT,
            )
            .await;
            assert!(matches!(
                strict_res,
                Err((
                    StoreError::StorageTimeout {
                        operation: "open",
                        ..
                    },
                    _
                ))
            ));

            let fallback = SessionStore::connect_runtime_or_memory_with_timeouts(
                "/tmp/stalled-fallback.db",
                bound,
                WRITE_TIMEOUT,
                READ_TIMEOUT,
            )
            .await
            .unwrap();
            let (path, reason) = fallback.memory_fallback().unwrap();
            assert_eq!(path, Path::new("/tmp/stalled-fallback.db"));
            assert!(reason.contains("timeout"), "{reason}");
        });
        drop(rt);
        TEST_STALL_OPEN.store(false, std::sync::atomic::Ordering::Release);
    }
}
