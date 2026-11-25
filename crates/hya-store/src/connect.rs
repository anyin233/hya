//! Session store connections, migrations, and exit draining.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

#[cfg(feature = "lifecycle-bench")]
use crate::bench;
use crate::runtime_owner::{
    RuntimeOwnerLock, RuntimeOwnerState, acquire_runtime_owner_lock, runtime_owner_lock_error,
};
use crate::{
    DatabasePaths, ExitDrainReport, OPEN_TIMEOUT, ProjectionCache, READ_TIMEOUT, SessionStore,
    StoreError, StoreHealth, WRITE_TIMEOUT, database_paths, error, validate_database_arg,
    write_queue,
};

// A busy disk can delay both pool acquisition and release of SQLite locks.
const FILE_POOL_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

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
pub(super) static TEST_STALL_OPEN: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(super) fn is_open_storage_fault(error: &StoreError) -> bool {
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

#[derive(Clone, Debug)]
pub(super) struct MemoryFallbackInfo {
    path: PathBuf,
    reason: String,
}

impl SessionStore {
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

    pub(super) async fn connect_runtime_bounded(
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
}
