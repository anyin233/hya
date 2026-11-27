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

mod bundle_claim;
mod bundle_state;
mod connect;
/// Typed store errors for the session store.
pub mod error;
mod file_blob;
mod handoff;
mod materialize;
mod paths;
mod permission;
mod preferences;
mod project;
mod projection_cache;
mod recovery;
mod runtime_owner;
mod write_queue;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hya_proto::{
    Envelope, Event, EventSeq, ProjectId, Projection, ProjectionReducer, ProjectionReducerRegistry,
    SessionId, now_millis,
};
use projection_cache::ProjectionCache;
use sqlx::Row;

use connect::MemoryFallbackInfo;
pub(crate) use connect::run_on_detached_thread;
use runtime_owner::RuntimeOwnerState;

pub use bundle_claim::BundleClaim;
pub use bundle_state::BundleStateEntry;
pub use error::StoreError;
pub use handoff::PendingResume;
pub use hya_proto::OwnerRunId;
pub use paths::user_cache_dir;
pub use permission::SavedPermission;
pub use preferences::{AgentEffortPreference, AgentModelPreference, ModelEffortPreference};
pub use project::{Project, ProjectSummary, normalize_project_path};
pub use recovery::{INTERRUPTED_REASON, InterruptedTurnRecovery, open_turn_terminal_events};
pub use runtime_owner::{DatabasePaths, database_paths, validate_database_arg};
pub use write_queue::{
    EXIT_DRAIN_TIMEOUT, ExitDrainReport, OPEN_TIMEOUT, READ_TIMEOUT, StoreHealth, WRITE_TIMEOUT,
};

/// SQLite-backed session event log, bundle storage/claims, and related tables.
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

    /// Configure the [`ProjectionReducerRegistry`] used when folding
    /// [`Event::Extension`] events on read and replay. Clears any stale
    /// in-process cache entries if the reducer signature changed.
    #[must_use]
    pub fn with_projection_reducers(self, reducers: ProjectionReducerRegistry) -> Self {
        self.projections.set_reducers(reducers);
        self
    }

    /// Replace the active [`ProjectionReducerRegistry`] for this store.
    pub fn set_projection_reducers(&self, reducers: ProjectionReducerRegistry) {
        self.projections.set_reducers(reducers);
    }

    /// Register one bundle [`ProjectionReducer`] on this store, invalidating
    /// in-process cached projections so subsequent reads fold with it.
    pub fn register_projection_reducer(&self, reducer: Arc<dyn ProjectionReducer>) {
        self.projections.register_reducer(reducer);
    }

    /// Return the currently active [`ProjectionReducerRegistry`].
    #[must_use]
    pub fn projection_reducers(&self) -> Arc<ProjectionReducerRegistry> {
        self.projections.reducers()
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

    /// Append an owned cross-session batch in order, returning envelopes only after commit.
    pub async fn append_events_batch(
        &self,
        batch: Vec<(SessionId, Event)>,
    ) -> Result<Vec<Envelope>, StoreError> {
        if let Some(queue) = &self.write_queue {
            return queue.submit(Vec::new(), batch).await;
        }
        let mut tx = self.begin_transaction().await?;
        let mut envelopes = Vec::with_capacity(batch.len());
        for (session, event) in batch {
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
    pub async fn warm_projection(&self, session: SessionId) -> Result<(), StoreError> {
        self.cached_projection(session).await.map(drop)
    }

    /// Fold one Session log inside an existing writer transaction, seeing
    /// uncommitted events appended in `tx`.
    pub async fn replay_projection_in_transaction(
        &self,
        tx: &mut StoreTransaction<'_>,
        session: SessionId,
    ) -> Result<Arc<Projection>, StoreError> {
        replay_projection(&self.projections, tx, session).await
    }

    /// Replay all persisted + uncommitted envelopes for `session` inside an open transaction.
    pub async fn replay_in_transaction(
        tx: &mut StoreTransaction<'_>,
        session: SessionId,
    ) -> Result<Vec<Envelope>, StoreError> {
        let rows =
            sqlx::query("SELECT seq, ts, payload FROM event_log WHERE session_id = ? ORDER BY seq")
                .bind(session.storage_key())
                .fetch_all(&mut **tx)
                .await?;
        let mut envelopes = Vec::with_capacity(rows.len());
        for row in rows {
            let seq: i64 = row.try_get("seq")?;
            let ts_millis: i64 = row.try_get("ts")?;
            let payload: String = row.try_get("payload")?;
            envelopes.push(Envelope {
                seq: EventSeq(seq.max(0) as u64),
                ts_millis,
                event: decode_event(&payload, session, seq)?,
            });
        }
        Ok(envelopes)
    }

    /// Find distinct session ids whose event log contains an [`Event::Extension`]
    /// matching `(bundle, kind)`.
    pub async fn find_sessions_with_extension_event(
        tx: &mut StoreTransaction<'_>,
        bundle: &str,
        kind: &str,
    ) -> Result<Vec<SessionId>, StoreError> {
        let rows = sqlx::query(
            "SELECT DISTINCT session_id FROM event_log \
             WHERE json_extract(payload, '$.type') = 'extension' \
               AND json_extract(payload, '$.bundle') = ? \
               AND json_extract(payload, '$.kind') = ? \
             ORDER BY session_id",
        )
        .bind(bundle)
        .bind(kind)
        .fetch_all(&mut **tx)
        .await?;
        rows.into_iter()
            .map(|row| {
                let key: Vec<u8> = row.try_get("session_id")?;
                decode_session_key(&key)
            })
            .collect()
    }

    async fn load_prepared_projection(
        &self,
        session: SessionId,
        base_override: Option<projection_cache::Base>,
    ) -> Result<(projection_cache::PreparedFold, u64, u64), StoreError> {
        let mut conn = self.read_pool_checked()?.acquire().await?;
        let reducers = self.projections.reducers();
        let signature = reducers.signature();
        let base = match base_override {
            Some(base) => Some(base),
            None => match self.projections.get(session) {
                Some(base) => Some(base),
                None => projection_cache::load_snapshot(&mut conn, session, &signature)
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
        let prepared = projection_cache::fold(
            &mut conn,
            session,
            base.map(|base| base.projection),
            reducers,
        )
        .await?;
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
            let signature = self.projections.reducers().signature();
            match projection.encode_snapshot_with(&signature) {
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
    if let Event::Extension { bundle, kind, .. } = event
        && (bundle.trim().is_empty() || kind.trim().is_empty())
    {
        return Err(StoreError::InvalidEvent(
            "extension event `bundle` and `kind` must be non-empty".to_owned(),
        ));
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
            let empty_reducers = Arc::new(ProjectionReducerRegistry::new());
            let base = projection_cache::load_snapshot(tx, session, "")
                .await?
                .map(Arc::new);
            let proj = projection_cache::fold(tx, session, base, empty_reducers)
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
    let reducers = cache.reducers();
    let signature = reducers.signature();
    let base = projection_cache::transaction_base(cache, tx, session, &signature).await?;
    let folded = projection_cache::fold(tx, session, base, reducers)
        .await?
        .finish();
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
mod event_contract_tests;
