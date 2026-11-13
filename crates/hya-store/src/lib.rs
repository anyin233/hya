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
mod bundle_registry;
/// Typed store errors shared by session and bundle registry APIs.
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
pub use bundle_registry::{
    BundleInstallAction, BundleInstallCandidate, BundleInstallOutcome, BundleInstallPlan,
    BundleRegistry, BundleRegistryRecord, BundleRegistrySnapshot, BundleUninstallOutcome,
    NamespaceInstallPolicy, UnreadableBundleRegistryRecord, is_downgrade,
};
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
/// Memory databases have no ownership files. File-backed URIs are rejected:
/// runtime writers must use a plain path with a canonical lock identity.
pub fn database_paths(db: &str) -> std::io::Result<Option<DatabasePaths>> {
    if db.is_empty() || db == ":memory:" {
        return Ok(None);
    }
    if db.starts_with("file:") || db.starts_with("sqlite:") {
        let memory_modes = |query: &str| {
            query
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .filter(|(key, _)| {
                    percent_encoding::percent_decode_str(key).eq(b"mode".iter().copied())
                })
                .map(|(_, value)| {
                    percent_encoding::percent_decode_str(value).eq(b"memory".iter().copied())
                })
                .collect::<Vec<_>>()
        };
        let (name, query) = db.split_once('?').unwrap_or((db, ""));
        let name = name
            .trim_start_matches("sqlite://")
            .trim_start_matches("sqlite:");
        let filename = percent_encoding::percent_decode_str(name)
            .decode_utf8()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        let outer_memory = memory_modes(query).contains(&true);
        // SQLx decodes the filename before SQLite interprets a native file: URI.
        let memory = if let Some(native) = filename.strip_prefix("file:") {
            let native = native.split_once('#').map_or(native, |(name, _)| name);
            let (name, query) = native.split_once('?').unwrap_or((native, ""));
            if query.is_empty() {
                percent_encoding::percent_decode_str(name).eq(b":memory:".iter().copied())
                    || outer_memory
            } else {
                let modes = memory_modes(query);
                !modes.is_empty() && modes.iter().all(|memory| *memory)
            }
        } else {
            filename == ":memory:" || outer_memory
        };
        if memory {
            return Ok(None);
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "only in-memory database URIs are supported; use a plain filesystem path for a file-backed database",
        ));
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
                    Ok(_) => {}
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

/// SQLite-backed session event log, resident claims, and related tables.
///
/// Construct with [`SessionStore::connect`] (file) or [`SessionStore::connect_memory`].
/// Projection is folded on read via `hya_proto::Projection` — there is no separate
/// materialized read model; folds are cached (in-process and as durable
/// snapshots) as a pure cache of that reducer, shared by every clone.
#[derive(Clone)]
pub struct SessionStore {
    pool: sqlx::SqlitePool,
    runtime_owner: Arc<RuntimeOwnerState>,
    projections: Arc<ProjectionCache>,
    read_only: bool,
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
    /// Open or create a file-backed store at `sqlite://{path}`, initialize the schema, enable WAL.
    /// Existing databases without the current baseline return [`StoreError::UnsupportedDatabase`].
    pub async fn connect(path: &str) -> Result<Self, StoreError> {
        let paths = database_paths(path)
            .map_err(|source| runtime_owner_lock_error(Some(Path::new(path)), source))?;
        Self::connect_file(path, paths, None).await
    }

    async fn connect_file(
        path: &str,
        paths: Option<DatabasePaths>,
        lock: Option<RuntimeOwnerLock>,
    ) -> Result<Self, StoreError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true);
        let opts = match paths.as_ref() {
            Some(paths) => opts.filename(&paths.database),
            None => opts,
        };
        let pool_options = SqlitePoolOptions::new().max_connections(8);
        #[cfg(feature = "lifecycle-bench")]
        let pool_options =
            pool_options.after_connect(|connection, _| Box::pin(bench::install(connection)));
        let pool = pool_options.connect_with(opts).await?;
        Self::migrate(&pool).await?;
        let runtime_owner = paths.map_or_else(RuntimeOwnerState::memory, RuntimeOwnerState::file);
        *runtime_owner
            .pending_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = lock;
        Ok(Self {
            pool,
            runtime_owner: Arc::new(runtime_owner),
            projections: Arc::new(ProjectionCache::new()),
            read_only: false,
            _memory_keeper: None,
        })
    }

    /// Reserve runtime ownership before SQLite initialization, then transfer the
    /// lock to the first `claim_runtime_owner` call on the returned store.
    pub async fn connect_runtime(path: &str) -> Result<Self, StoreError> {
        let paths = database_paths(path)
            .map_err(|source| runtime_owner_lock_error(Some(Path::new(path)), source))?;
        let lock = paths.as_ref().map(acquire_runtime_owner_lock).transpose()?;
        Self::connect_file(path, paths, lock).await
    }

    /// Open an existing database read-only, without initialization or ownership.
    /// SQLite rejects a missing file; projection reads never persist snapshots.
    pub async fn connect_read_only(path: &str) -> Result<Self, StoreError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))?
            .read_only(true)
            .create_if_missing(false)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        Ok(Self {
            pool,
            runtime_owner: Arc::new(RuntimeOwnerState::memory()),
            projections: Arc::new(ProjectionCache::new()),
            read_only: true,
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
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::migrate(&pool).await?;
        Ok(Self {
            pool,
            runtime_owner: Arc::new(RuntimeOwnerState::memory()),
            projections: Arc::new(ProjectionCache::new()),
            read_only: false,
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
    /// until every clone sharing this store is dropped. In-memory stores retain the
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
    pub async fn begin_transaction(
        &self,
    ) -> Result<sqlx::Transaction<'_, sqlx::Sqlite>, StoreError> {
        Ok(self.pool.begin_with("BEGIN IMMEDIATE").await?)
    }

    /// Append one domain event to the session log; returns the assigned sequence.
    pub async fn append_event(
        &self,
        session: SessionId,
        event: &Event,
    ) -> Result<(EventSeq, i64), StoreError> {
        let mut tx = self.pool.begin().await?;
        let envelope = append_event_in_transaction(&mut tx, session, event.clone()).await?;
        tx.commit().await?;
        Ok((envelope.seq, envelope.ts_millis))
    }

    /// Append an owned batch in order, returning envelopes only after commit.
    pub async fn append_events(
        &self,
        session: SessionId,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, StoreError> {
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
        let key = session.storage_key();
        let rows =
            sqlx::query("SELECT seq, ts, payload FROM event_log WHERE session_id = ? ORDER BY seq")
                .bind(key)
                .fetch_all(&self.pool)
                .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let seq: i64 = r.try_get("seq")?;
            let ts: i64 = r.try_get("ts")?;
            let payload: String = r.try_get("payload")?;
            let event = decode_event(&payload, session, seq)?;
            out.push(Envelope {
                seq: EventSeq(seq.max(0) as u64),
                ts_millis: ts,
                event,
            });
        }
        Ok(out)
    }

    /// Read a sequence page without decoding the preceding event log.
    /// A zero limit requests the entire remaining tail.
    pub async fn replay_page(
        &self,
        session: SessionId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<Envelope>, StoreError> {
        let rows = sqlx::query(
            "SELECT seq, ts, payload FROM event_log WHERE session_id = ? AND seq > ? ORDER BY seq LIMIT ?",
        )
        .bind(session.storage_key())
        .bind(i64::try_from(after).unwrap_or(i64::MAX))
        .bind(if limit == 0 { -1 } else { i64::from(limit) })
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let seq: i64 = row.try_get("seq")?;
                let payload: String = row.try_get("payload")?;
                Ok(Envelope {
                    seq: EventSeq(seq.max(0) as u64),
                    ts_millis: row.try_get("ts")?,
                    event: decode_event(&payload, session, seq)?,
                })
            })
            .collect()
    }

    /// Return whether the event log contains a Session without replaying it.
    ///
    /// # Errors
    /// Returns SQLite failures.
    pub async fn session_exists(&self, session: SessionId) -> Result<bool, StoreError> {
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT EXISTS(SELECT 1 FROM event_log WHERE session_id = ? LIMIT 1)",
        )
        .bind(session.storage_key())
        .fetch_one(&self.pool)
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
        let mut tx = self.pool.begin().await?;
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

    /// Fold through the cache: cached base (in-process, else the durable
    /// snapshot) plus the events after it; persist a durable snapshot once
    /// enough events were folded since the last one.
    async fn cached_projection(&self, session: SessionId) -> Result<Arc<Projection>, StoreError> {
        let mut conn = self.pool.acquire().await?;
        let base = match self.projections.get(session) {
            Some(base) => Some(base),
            None => projection_cache::load_snapshot(&mut conn, session)
                .await?
                .map(|projection| projection_cache::Base {
                    persisted_seq: projection.last_seq,
                    unpersisted: 0,
                    projection: Arc::new(projection),
                }),
        };
        let (persisted_seq, unpersisted) = base
            .as_ref()
            .map_or((0, 0), |base| (base.persisted_seq, base.unpersisted));
        let prepared =
            projection_cache::fold(&mut conn, session, base.map(|base| base.projection)).await?;
        let (folded, persisted_seq, unpersisted) =
            self.projections
                .advance(session, prepared, persisted_seq, unpersisted);
        let last_seq = folded.projection.last_seq;
        if last_seq == 0 {
            return Ok(folded.projection);
        }
        if !self.read_only
            && last_seq > persisted_seq
            && unpersisted >= self.projections.snapshot_interval()
        {
            // Best effort: a failed write only means a later read folds more.
            match projection_cache::persist_snapshot(&mut conn, session, &folded.projection).await {
                Ok(()) => {
                    self.projections
                        .put(session, Arc::clone(&folded.projection), last_seq, 0);
                }
                Err(error) => {
                    tracing::debug!(%session, "projection snapshot not persisted: {error}");
                }
            }
        }
        Ok(folded.projection)
    }

    /// Sessions present in the event log, newest-updated first.
    pub async fn list_sessions(&self) -> Result<Vec<SessionInfo>, StoreError> {
        let rows = sqlx::query(
            "SELECT session_id, MIN(ts) AS started, MAX(ts) AS updated, COUNT(*) AS n \
             FROM event_log GROUP BY session_id ORDER BY updated DESC, session_id DESC",
        )
        .fetch_all(&self.pool)
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
        .fetch_all(&self.pool)
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
        .fetch_all(&self.pool)
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
        .fetch_all(&self.pool)
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
        .fetch_one(&self.pool)
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

/// Append one event inside an existing writer transaction.
pub async fn append_event_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session: SessionId,
    event: Event,
) -> Result<Envelope, StoreError> {
    if let Event::MailSent {
        from,
        to: hya_proto::MailEndpoint::Handle(handle),
        ..
    } = &event
        && from != hya_proto::scope::HARNESS_HANDLE
    {
        return Err(StoreError::MailAuthorizationRejected {
            sender: from.clone(),
            channel: format!("direct:{handle}"),
            reason: "agent mail must target a channel".into(),
        });
    }
    let ts_millis = now_millis();
    let payload = serde_json::to_string(&event)?;
    let row = sqlx::query(
        "INSERT INTO event_log (session_id, payload, ts) VALUES (?, ?, ?) RETURNING seq",
    )
    .bind(session.storage_key())
    .bind(payload)
    .bind(ts_millis)
    .fetch_one(&mut **tx)
    .await?;
    let seq: i64 = row.try_get("seq")?;
    materialize::materialize_event_side_tables(tx, session, &event, ts_millis).await?;
    match &event {
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
            let projection = projection_cache::fold(tx, session, base)
                .await?
                .finish()
                .projection;
            if projection.session.id.is_some()
                && projection.session.parent.is_none()
                && !projection.session.is_archived()
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
    Ok(Envelope {
        seq: EventSeq(seq.max(0) as u64),
        ts_millis,
        event,
    })
}

/// Fold one Session log inside an existing writer transaction.
///
/// Starts from the cached fold (anchor-checked through the transaction) and
/// sees the transaction's own uncommitted events, so the result is never
/// written back to the cache.
pub(crate) async fn replay_projection(
    cache: &ProjectionCache,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod event_contract_tests {
    use super::*;

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
            .execute(&store.pool)
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
                .bind(actor.storage_key()).bind(owner.storage_key()).execute(&store.pool).await.unwrap();
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
                    .execute(&store.pool)
                    .await
                    .unwrap();
            }
            sqlx::query("INSERT INTO resident_actor_claim (actor_id, root_session_id, epoch, owner_run_id, state) VALUES (?, ?, 1, randomblob(16), 'active')")
                .bind(session.storage_key())
                .bind(parent.unwrap_or(session).storage_key())
                .execute(&store.pool).await.unwrap();
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
            sqlx::query(sql).execute(&store.pool).await.unwrap();
        }
        // A late failure must roll back the event and auxiliary deletes too.
        sqlx::query("CREATE TRIGGER reject_delete BEFORE DELETE ON session BEGIN SELECT RAISE(ABORT, 'delete blocked'); END")
            .execute(&store.pool).await.unwrap();
        assert!(store.delete_session(root).await.is_err());
        assert_eq!(store.replay(root).await.unwrap().len(), 2);
        assert!(store.file_blob(child, "hash").await.unwrap().is_some());
        sqlx::query("DROP TRIGGER reject_delete")
            .execute(&store.pool)
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
                        .fetch_one(&store.pool)
                        .await
                        .unwrap();
                assert_eq!(count, 0, "{table} retains {session}");
            }
            let count: i64 =
                sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?"))
                    .bind(other.storage_key())
                    .fetch_one(&store.pool)
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
                .fetch_one(&store.pool)
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
            .execute(&store.pool)
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
            .execute(&store.pool)
            .await
            .unwrap();
        let error = store.list_sessions().await.unwrap_err();
        assert!(matches!(error, StoreError::SessionKeyData(_)), "{error}");
        assert!(error.to_string().contains("hysec_"), "{error}");
        assert!(decode_session_key(&[0xff; 16]).is_err());
    }
}
