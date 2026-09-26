//! `hya-store` — SQLite event log + replay; projection folded on read via the
//! shared `hya_proto::Projection` reducer (one reducer, no SQL/reducer
//! divergence). Folds are cached in-process and as durable snapshots
//! (`projection_cache`), a pure cache of that reducer: a read folds only the
//! events after the cached projection and always equals a full replay.
//!
//! NOTE: PRAGMAs (WAL etc.) are set via connect options, NOT a migration — `WAL`
//! cannot run inside the transaction sqlx wraps migrations in.

mod admission;
mod agent_model_preference;
mod bundle_registry;
/// Typed store errors shared by session and bundle registry APIs.
pub mod error;
mod file_blob;
mod mailbox;
mod materialize;
mod permission;
mod project;
mod projection_cache;
mod recovery;
mod resident_claim;
mod sync;
mod workflow;

/// Upper bound on durable `spawn_intent` bytes (1 MiB); mirrored by SQL CHECK.
pub const MAX_ADMISSION_INTENT_BYTES: usize = 1_048_576;

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hya_proto::{Envelope, Event, EventSeq, ProjectId, Projection, SessionId, now_millis};
use projection_cache::ProjectionCache;
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub use admission::{
    AdmissionActorBinding, AdmissionBatchClaimOutcome, AdmissionClaim, AdmissionClaimOutcome,
    AdmissionCounts, AdmissionFinalizeOutcome, AdmissionIntent, AdmissionLaunch, AdmissionRecord,
    AdmissionReleaseOutcome, AdmissionStartOutcome, AdmissionState, AdmissionTerminal,
};
pub use agent_model_preference::AgentModelPreference;
pub use bundle_registry::{
    BundleInstallAction, BundleInstallCandidate, BundleInstallOutcome, BundleInstallPlan,
    BundleRegistry, BundleRegistryRecord, BundleRegistrySnapshot, BundleUninstallOutcome,
    NamespaceInstallPolicy, is_downgrade,
};
pub use error::StoreError;
pub use hya_proto::{ActorClaim, OwnerRunId};
pub use mailbox::{RecoveredResidentOutcome, RecoveredResidentWork};
pub use permission::SavedPermission;
pub use project::{Project, ProjectSummary, normalize_project_path};
pub use recovery::{INTERRUPTED_REASON, InterruptedTurnRecovery};
pub use resident_claim::RecoveredActorClaim;
pub use workflow::{WorkflowAdmissionOutcome, WorkflowSelectionOutcome};

struct RuntimeOwnerState {
    lock_path: Option<PathBuf>,
    claim: Mutex<Option<RuntimeOwnerClaim>>,
}

struct RuntimeOwnerClaim {
    owner: OwnerRunId,
    _lock_file: Option<File>,
}

impl RuntimeOwnerState {
    fn memory() -> Self {
        Self {
            lock_path: None,
            claim: Mutex::new(None),
        }
    }

    fn file(lock_path: PathBuf) -> Self {
        Self {
            lock_path: Some(lock_path),
            claim: Mutex::new(None),
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

        let lock_file = self
            .lock_path
            .as_deref()
            .map(acquire_runtime_owner_lock)
            .transpose()?;
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
}

fn runtime_owner_lock_path(sqlite_path: &Path) -> PathBuf {
    let mut lock_path = OsString::from(sqlite_path.as_os_str());
    lock_path.push(".runtime-owner.lock");
    PathBuf::from(lock_path)
}

fn runtime_owner_lock_error(path: Option<&Path>, source: std::io::Error) -> StoreError {
    StoreError::RuntimeOwnerLock {
        path: path.map_or_else(|| PathBuf::from("<memory>"), Path::to_path_buf),
        source: Arc::new(source),
    }
}

fn acquire_runtime_owner_lock(path: &Path) -> Result<File, StoreError> {
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
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(StoreError::RuntimeOwnerBusy),
        Err(TryLockError::Error(source)) => Err(runtime_owner_lock_error(Some(path), source)),
    }
}

fn canonical_runtime_owner_lock_path(sqlite_path: &str) -> Result<PathBuf, StoreError> {
    let path = Path::new(sqlite_path);
    let canonical = fs::canonicalize(path)
        .map_err(|source| runtime_owner_lock_error(Some(&runtime_owner_lock_path(path)), source))?;
    Ok(runtime_owner_lock_path(&canonical))
}

/// SQLite-backed session event log, token ledger, admission journal, and related tables.
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

/// One token-usage row written by the engine after a completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerEntry {
    /// Session the usage belongs to.
    pub session: SessionId,
    /// Role label (e.g. assistant / system accounting bucket).
    pub role: String,
    /// Optional multi-step iteration index.
    pub iteration: Option<i64>,
    /// Optional id correlating multiple ledger rows for one completion run.
    pub completion_run_id: Option<String>,
    /// Prompt-side token count.
    pub prompt_tokens: i64,
    /// Completion-side token count.
    pub completion_tokens: i64,
    /// Confidence or estimation quality label stored with the row.
    pub confidence: String,
    /// Provider id the completion ran on (e.g. `12th` in `12th/glm-5.3`).
    pub provider: Option<String>,
    /// Full model ref the completion ran on.
    pub model: Option<String>,
}

impl SessionStore {
    /// Open or create a file-backed store at `sqlite://{path}`, run migrations, enable WAL.
    pub async fn connect(path: &str) -> Result<Self, StoreError> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        Self::migrate(&pool).await?;
        let lock_path = canonical_runtime_owner_lock_path(path)?;
        Ok(Self {
            pool,
            runtime_owner: Arc::new(RuntimeOwnerState::file(lock_path)),
            projections: Arc::new(ProjectionCache::new()),
        })
    }

    /// Open an in-memory store (single connection) and run migrations.
    pub async fn connect_memory() -> Result<Self, StoreError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::migrate(&pool).await?;
        Ok(Self {
            pool,
            runtime_owner: Arc::new(RuntimeOwnerState::memory()),
            projections: Arc::new(ProjectionCache::new()),
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

    /// Require a matching runtime-owner claim before a startup-only mutation.
    pub(crate) fn require_runtime_owner(&self, owner: OwnerRunId) -> Result<(), StoreError> {
        self.runtime_owner.require(owner)
    }

    async fn migrate(pool: &sqlx::SqlitePool) -> Result<(), StoreError> {
        sqlx::migrate!("./migrations").run(pool).await?;
        Ok(())
    }

    /// Append one domain event to the session log; returns the assigned sequence.
    pub async fn append_event(
        &self,
        session: SessionId,
        event: &Event,
    ) -> Result<(EventSeq, i64), StoreError> {
        let payload = serde_json::to_string(event)?;
        let key = session.storage_key();
        let ts_millis = now_millis();
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "INSERT INTO event_log (session_id, payload, ts) VALUES (?, ?, ?) RETURNING seq, ts",
        )
        .bind(&key)
        .bind(payload)
        .bind(ts_millis)
        .fetch_one(&mut *tx)
        .await?;
        materialize::materialize_event_side_tables(&mut tx, session, event, ts_millis).await?;
        tx.commit().await?;
        let seq: i64 = row.try_get("seq")?;
        let ts: i64 = row.try_get("ts")?;
        Ok((EventSeq(seq.max(0) as u64), ts))
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
            let event: Event = serde_json::from_str(&payload)?;
            out.push(Envelope {
                seq: EventSeq(seq.max(0) as u64),
                ts_millis: ts,
                event,
            });
        }
        Ok(out)
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

    /// Delete ledger and event rows for a session; returns whether any event rows were removed.
    pub async fn delete_session(&self, session: SessionId) -> Result<bool, StoreError> {
        let key = session.storage_key();
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM token_ledger WHERE session_id = ?")
            .bind(key.clone())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM open_assistant_message WHERE session_id = ?")
            .bind(key.clone())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM projection_snapshot WHERE session_id = ?")
            .bind(key.clone())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM file_blob WHERE session_id = ?")
            .bind(key.clone())
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query("DELETE FROM event_log WHERE session_id = ?")
            .bind(key)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.projections.remove(session);
        Ok(result.rows_affected() > 0)
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
        let (mut persisted_seq, mut unpersisted) = base
            .as_ref()
            .map_or((0, 0), |base| (base.persisted_seq, base.unpersisted));
        let folded =
            projection_cache::fold(&mut conn, session, base.map(|base| base.projection)).await?;
        if folded.rebuilt {
            persisted_seq = 0;
            unpersisted = folded.applied;
        } else {
            unpersisted = unpersisted.saturating_add(folded.applied);
        }
        let last_seq = folded.projection.last_seq;
        if last_seq == 0 {
            self.projections.remove(session);
            return Ok(folded.projection);
        }
        if last_seq > persisted_seq && unpersisted >= self.projections.snapshot_interval() {
            // Best effort: a failed write only means a later read folds more.
            match projection_cache::persist_snapshot(&mut conn, session, &folded.projection).await {
                Ok(()) => {
                    persisted_seq = last_seq;
                    unpersisted = 0;
                }
                Err(error) => {
                    tracing::debug!(%session, "projection snapshot not persisted: {error}");
                }
            }
        }
        self.projections.put(
            session,
            Arc::clone(&folded.projection),
            persisted_seq,
            unpersisted,
        );
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

    /// Insert one token-ledger row (new UUID primary key, current timestamp).
    pub async fn record_usage(&self, entry: &LedgerEntry) -> Result<(), StoreError> {
        let id = uuid::Uuid::now_v7().as_bytes().to_vec();
        let session = entry.session.storage_key();
        sqlx::query(
            "INSERT INTO token_ledger \
             (id, session_id, iteration, completion_run_id, role, prompt_tokens, completion_tokens, confidence, provider, model, ts) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(session)
        .bind(entry.iteration)
        .bind(entry.completion_run_id.clone())
        .bind(entry.role.clone())
        .bind(entry.prompt_tokens)
        .bind(entry.completion_tokens)
        .bind(entry.confidence.clone())
        .bind(entry.provider.clone())
        .bind(entry.model.clone())
        .bind(now_millis())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// All token-ledger rows for a session in timestamp order.
    pub async fn read_usage(&self, session: SessionId) -> Result<Vec<LedgerEntry>, StoreError> {
        let key = session.storage_key();
        let rows = sqlx::query(
            "SELECT iteration, completion_run_id, role, prompt_tokens, completion_tokens, confidence, provider, model \
             FROM token_ledger WHERE session_id = ? ORDER BY ts",
        )
        .bind(key)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(LedgerEntry {
                session,
                iteration: r.try_get("iteration")?,
                completion_run_id: r.try_get("completion_run_id")?,
                role: r.try_get("role")?,
                prompt_tokens: r.try_get("prompt_tokens")?,
                completion_tokens: r.try_get("completion_tokens")?,
                confidence: r.try_get("confidence")?,
                provider: r.try_get("provider")?,
                model: r.try_get("model")?,
            });
        }
        Ok(out)
    }
}

/// Append one event inside an existing writer transaction.
pub(crate) async fn append_event_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session: SessionId,
    event: Event,
) -> Result<Envelope, StoreError> {
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
) -> Result<Projection, StoreError> {
    let base = projection_cache::transaction_base(cache, tx, session).await?;
    let folded = projection_cache::fold(tx, session, base).await?;
    Ok(Arc::unwrap_or_clone(folded.projection))
}

/// Decode `list_sessions`-shaped rows (`session_id`, `started`, `updated`,
/// `n`), skipping undecodable session keys.
fn session_infos(rows: Vec<sqlx::sqlite::SqliteRow>) -> Result<Vec<SessionInfo>, StoreError> {
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let key: Vec<u8> = r.try_get("session_id")?;
        let started: i64 = r.try_get("started")?;
        let updated: i64 = r.try_get("updated")?;
        let n: i64 = r.try_get("n")?;
        if let Some(session) = decode_session_key(&key) {
            out.push(SessionInfo {
                session,
                started_millis: started,
                updated_millis: updated,
                events: n.max(0) as u64,
            });
        }
    }
    Ok(out)
}

pub(crate) fn decode_session_key(key: &[u8]) -> Option<SessionId> {
    if let Ok(raw) = std::str::from_utf8(key) {
        return raw.parse().ok();
    }
    uuid::Uuid::from_slice(key).ok().map(SessionId::from_uuid)
}
