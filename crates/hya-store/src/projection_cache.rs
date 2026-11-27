//! Replay-consistent projection cache.
//!
//! `read_projection` used to decode and fold a session's whole event log on
//! every call. This module keeps the shared reducer's output cached at two
//! levels, both pure caches of the event log (never a source of truth):
//!
//! - an in-process map `session -> Arc<Projection>` shared by every clone of a
//!   [`crate::SessionStore`], advanced on read by folding only the events
//!   after the cached projection's `last_seq`;
//! - durable rows in `projection_snapshot`, written every
//!   [`DEFAULT_SNAPSHOT_INTERVAL`] newly folded events, so a restarted process
//!   folds only the tail instead of the whole log.
//!
//! Invariants that make "cached base + tail" equal a full replay:
//!
//! - `event_log` is append-only per session (the only removal is
//!   `delete_session`, which drops every row of the session and its
//!   descendants) and `seq` is a
//!   global AUTOINCREMENT assigned under SQLite's single writer, so commit
//!   order equals `seq` order and every event with `seq <= last_seq` of a
//!   session is visible once the event at `last_seq` is.
//! - Every fold re-reads the base's anchor event (`seq = last_seq` of the same
//!   session). A missing anchor — deleted session, restored older database —
//!   discards the base and folds the full log.
//! - A durable snapshot is trusted only under the running
//!   [`PROJECTION_REDUCER_VERSION`] and only if it decodes; otherwise it is
//!   ignored and later overwritten.
//! - Folds inside a writer transaction may see the transaction's own
//!   uncommitted events, so they read the cache but never write it back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use futures::TryStreamExt as _;
use hya_proto::{
    Envelope, EventSeq, PROJECTION_REDUCER_VERSION, Projection, ProjectionReducer,
    ProjectionReducerRegistry, SessionId,
};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row as _, SqliteConnection};

use crate::StoreError;

/// Newly folded events after which a read persists a durable snapshot.
pub(crate) const DEFAULT_SNAPSHOT_INTERVAL: u64 = 1_024;

/// Sessions kept in the in-process cache; the least recently read is evicted.
const MAX_CACHED_PROJECTIONS: usize = 256;

/// Shared in-process projection cache (one per opened database).
pub(crate) struct ProjectionCache {
    entries: Mutex<HashMap<SessionId, Entry>>,
    reducers: RwLock<Arc<ProjectionReducerRegistry>>,
    clock: AtomicU64,
    snapshot_interval: AtomicU64,
}

#[derive(Clone)]
struct Entry {
    projection: Arc<Projection>,
    /// `last_seq` of the durable snapshot row this process last wrote or
    /// loaded for the session (0: none known).
    persisted_seq: u64,
    /// Events folded since that durable snapshot.
    unpersisted: u64,
    last_used: u64,
}

/// Starting point for one fold.
pub(crate) struct Base {
    pub(crate) projection: Arc<Projection>,
    pub(crate) persisted_seq: u64,
    pub(crate) unpersisted: u64,
}

/// Database read completed; applying the decoded tail requires no await.
pub(crate) struct PreparedFold {
    projection: Arc<Projection>,
    tail: Vec<Envelope>,
    reducers: Arc<ProjectionReducerRegistry>,
    applied: u64,
    rebuilt: bool,
}

impl PreparedFold {
    pub(crate) fn final_seq(&self) -> u64 {
        self.tail
            .last()
            .map_or(self.projection.last_seq, |env| env.seq.0)
    }

    pub(crate) fn finish(mut self) -> Folded {
        if !self.tail.is_empty() {
            let projection = Arc::make_mut(&mut self.projection);
            for envelope in self.tail {
                projection.apply_with(&envelope, &self.reducers);
            }
        }
        Folded {
            projection: self.projection,
            applied: self.applied,
            rebuilt: self.rebuilt,
        }
    }
}

/// Result of folding a session log onto a base.
pub(crate) struct Folded {
    pub(crate) projection: Arc<Projection>,
    /// Events applied on top of the base (the whole log when `rebuilt`).
    pub(crate) applied: u64,
    /// The base was absent or its anchor was gone: the whole log was folded.
    pub(crate) rebuilt: bool,
}

impl ProjectionCache {
    pub(crate) fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            reducers: RwLock::new(Arc::new(ProjectionReducerRegistry::new())),
            clock: AtomicU64::new(0),
            snapshot_interval: AtomicU64::new(DEFAULT_SNAPSHOT_INTERVAL),
        }
    }

    pub(crate) fn reducers(&self) -> Arc<ProjectionReducerRegistry> {
        Arc::clone(
            &self
                .reducers
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    pub(crate) fn set_reducers(&self, next: ProjectionReducerRegistry) {
        let mut entries = self.entries();
        let mut guard = self
            .reducers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.signature() != next.signature() {
            entries.clear();
        }
        *guard = Arc::new(next);
    }

    pub(crate) fn register_reducer(&self, reducer: Arc<dyn ProjectionReducer>) {
        let mut entries = self.entries();
        let mut guard = self
            .reducers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next = (**guard).clone();
        next.register(reducer);
        entries.clear();
        *guard = Arc::new(next);
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn snapshot_interval(&self) -> u64 {
        self.snapshot_interval.load(Ordering::Relaxed)
    }

    pub(crate) fn set_snapshot_interval(&self, events: u64) {
        self.snapshot_interval
            .store(events.max(1), Ordering::Relaxed);
    }

    /// The cached base for `session`, if any.
    pub(crate) fn get(&self, session: SessionId) -> Option<Base> {
        let now = self.tick();
        let mut entries = self.entries();
        let entry = entries.get_mut(&session)?;
        entry.last_used = now;
        Some(Base {
            projection: Arc::clone(&entry.projection),
            persisted_seq: entry.persisted_seq,
            unpersisted: entry.unpersisted,
        })
    }

    /// Update snapshot durability counters only if `session` is still cached at `last_seq`.
    /// Never re-inserts a fold that was evicted or invalidated during snapshot encoding.
    pub(crate) fn mark_persisted(&self, session: SessionId, last_seq: u64) {
        let mut entries = self.entries();
        if let Some(entry) = entries.get_mut(&session)
            && entry.projection.last_seq == last_seq
        {
            entry.persisted_seq = last_seq;
            entry.unpersisted = 0;
        }
    }

    fn insert(
        &self,
        entries: &mut HashMap<SessionId, Entry>,
        session: SessionId,
        projection: Arc<Projection>,
        persisted_seq: u64,
        unpersisted: u64,
    ) {
        let now = self.tick();
        if let Some(existing) = entries.get(&session)
            && existing.projection.last_seq > projection.last_seq
        {
            return;
        }
        if !entries.contains_key(&session)
            && entries.len() >= MAX_CACHED_PROJECTIONS
            && let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
        {
            entries.remove(&oldest);
        }
        entries.insert(
            session,
            Entry {
                projection,
                persisted_seq,
                unpersisted,
                last_used: now,
            },
        );
    }

    /// Advance an active cached fold (or initialize one from `SessionCreated`).
    pub(crate) fn apply_envelope(&self, session: SessionId, env: &Envelope, init_on_create: bool) {
        let now = self.tick();
        let reducers = self.reducers();
        let mut entries = self.entries();
        if let Some(entry) = entries.get_mut(&session) {
            if env.seq.0 > entry.projection.last_seq {
                Arc::make_mut(&mut entry.projection).apply_with(env, &reducers);
                entry.unpersisted = entry.unpersisted.saturating_add(1);
                entry.last_used = now;
            }
            return;
        }
        if init_on_create && matches!(env.event, hya_proto::Event::SessionCreated { .. }) {
            let proj = Arc::new(Projection::from_events_with(
                std::slice::from_ref(env),
                &reducers,
            ));
            self.insert(&mut entries, session, proj, 0, 1);
        }
    }

    /// Transfer cache ownership, apply the tail and publish without an await or cache gap.
    pub(crate) fn advance(
        &self,
        session: SessionId,
        prepared: PreparedFold,
        mut persisted_seq: u64,
        mut unpersisted: u64,
        tail: &[Envelope],
    ) -> (Folded, u64, u64) {
        let reducers = Arc::clone(&prepared.reducers);
        let mut entries = self.entries();
        if entries
            .get(&session)
            .is_some_and(|entry| Arc::ptr_eq(&entry.projection, &prepared.projection))
        {
            entries.remove(&session);
        }
        let mut folded = prepared.finish();
        if folded.rebuilt {
            persisted_seq = 0;
            unpersisted = folded.applied;
        } else {
            unpersisted = unpersisted.saturating_add(folded.applied);
        }
        unpersisted =
            unpersisted.saturating_add(apply_tail(&mut folded.projection, tail, &reducers));
        if let Some(existing) = entries.get_mut(&session)
            && existing.projection.last_seq >= folded.projection.last_seq
        {
            existing.last_used = self.tick();
            let last_seq = existing.projection.last_seq;
            return (
                Folded {
                    projection: Arc::clone(&existing.projection),
                    applied: 0,
                    rebuilt: false,
                },
                last_seq,
                0,
            );
        }
        if folded.projection.last_seq == 0 {
            entries.remove(&session);
        } else {
            self.insert(
                &mut entries,
                session,
                Arc::clone(&folded.projection),
                persisted_seq,
                unpersisted,
            );
        }
        (folded, persisted_seq, unpersisted)
    }

    /// Apply `tail` on top of `prepared` without inserting or updating the cache.
    pub(crate) fn finish_without_caching(
        &self,
        prepared: PreparedFold,
        tail: &[Envelope],
    ) -> Folded {
        let reducers = Arc::clone(&prepared.reducers);
        let mut folded = prepared.finish();
        folded.applied =
            folded
                .applied
                .saturating_add(apply_tail(&mut folded.projection, tail, &reducers));
        folded
    }

    /// Drop every entry for `sessions` (e.g. a deleted session and its
    /// descendants); unknown sessions are ignored.
    pub(crate) fn remove_all(&self, sessions: impl IntoIterator<Item = SessionId>) {
        let mut entries = self.entries();
        for session in sessions {
            entries.remove(&session);
        }
    }
}

fn apply_tail(
    projection: &mut Arc<Projection>,
    tail: &[Envelope],
    reducers: &ProjectionReducerRegistry,
) -> u64 {
    let last_seq = projection.last_seq;
    if !tail.iter().any(|env| env.seq.0 > last_seq) {
        return 0;
    }
    let mut applied = 0_u64;
    let proj = Arc::make_mut(projection);
    for env in tail {
        if env.seq.0 > proj.last_seq {
            proj.apply_with(env, reducers);
            applied = applied.saturating_add(1);
        }
    }
    applied
}

fn seq_param(seq: u64) -> i64 {
    i64::try_from(seq).unwrap_or(i64::MAX)
}

fn row_seq(row: &SqliteRow) -> Result<u64, StoreError> {
    let seq: i64 = row.try_get("seq")?;
    Ok(seq.max(0) as u64)
}

fn decode_row(row: &SqliteRow, session: SessionId) -> Result<Envelope, StoreError> {
    let ts_millis: i64 = row.try_get("ts")?;
    let payload: &str = row.try_get("payload")?;
    let event = crate::decode_event(payload, session, row_seq(row)? as i64)?;
    Ok(Envelope {
        seq: EventSeq(row_seq(row)?),
        ts_millis,
        event,
    })
}

/// The durable snapshot of `session`, when one exists under the running
/// reducer version and signature and decodes.
pub(crate) async fn load_snapshot(
    conn: &mut SqliteConnection,
    session: SessionId,
    expected_signature: &str,
) -> Result<Option<Projection>, StoreError> {
    let row = sqlx::query(
        "SELECT reducer_version, last_seq, payload FROM projection_snapshot WHERE session_id = ?",
    )
    .bind(session.storage_key())
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let version: i64 = row.try_get("reducer_version")?;
    if version != i64::from(PROJECTION_REDUCER_VERSION) {
        return Ok(None);
    }
    let last_seq: i64 = row.try_get("last_seq")?;
    let payload: &[u8] = row.try_get("payload")?;
    match Projection::decode_snapshot_with(payload, expected_signature) {
        Ok(projection) if seq_param(projection.last_seq) == last_seq => Ok(Some(projection)),
        Ok(_) => Ok(None),
        Err(error) => {
            tracing::debug!(%session, "ignoring undecodable projection snapshot: {error}");
            Ok(None)
        }
    }
}

/// Fold `session`'s log onto `base`, or the whole log when `base` is absent,
/// empty, or no longer anchored in the log.
pub(crate) async fn fold(
    conn: &mut SqliteConnection,
    session: SessionId,
    base: Option<Arc<Projection>>,
    reducers: Arc<ProjectionReducerRegistry>,
) -> Result<PreparedFold, StoreError> {
    let key = session.storage_key();
    if let Some(base) = base.filter(|base| base.last_seq > 0) {
        let from = base.last_seq;
        let mut anchored = false;
        let mut tail = Vec::new();
        let mut applied = 0_u64;
        {
            let mut rows = sqlx::query(
                "SELECT seq, ts, payload FROM event_log \
                 WHERE session_id = ? AND seq >= ? ORDER BY seq",
            )
            .bind(&key)
            .bind(seq_param(from))
            .fetch(&mut *conn);
            while let Some(row) = rows.try_next().await? {
                if !anchored {
                    if row_seq(&row)? != from {
                        break;
                    }
                    anchored = true;
                    continue;
                }
                let envelope = decode_row(&row, session)?;
                tail.push(envelope);
                applied += 1;
            }
        }
        if anchored {
            return Ok(PreparedFold {
                projection: base,
                tail,
                reducers,
                applied,
                rebuilt: false,
            });
        }
    }
    let mut projection = Projection::default();
    let mut applied = 0_u64;
    {
        let mut rows =
            sqlx::query("SELECT seq, ts, payload FROM event_log WHERE session_id = ? ORDER BY seq")
                .bind(&key)
                .fetch(&mut *conn);
        while let Some(row) = rows.try_next().await? {
            projection.apply_with(&decode_row(&row, session)?, &reducers);
            applied += 1;
        }
    }
    Ok(PreparedFold {
        projection: Arc::new(projection),
        tail: Vec::new(),
        reducers,
        applied,
        rebuilt: true,
    })
}

/// Upsert a pre-encoded durable snapshot for `session` at `last_seq`.
///
/// Written only while the anchor event still exists, so a snapshot racing a
/// `delete_session` is a no-op. Replacing a newer row with an older (still
/// anchored) fold is harmless: any anchored prefix is a valid base.
pub(crate) async fn persist_snapshot(
    conn: &mut SqliteConnection,
    session: SessionId,
    last_seq: u64,
    payload: Vec<u8>,
) -> Result<(), StoreError> {
    let key = session.storage_key();
    let last_seq = seq_param(last_seq);
    sqlx::query(
        "INSERT INTO projection_snapshot (session_id, reducer_version, last_seq, payload) \
         SELECT ?1, ?2, ?3, ?4 \
         WHERE EXISTS (SELECT 1 FROM event_log WHERE seq = ?3 AND session_id = ?1) \
         ON CONFLICT(session_id) DO UPDATE SET \
             reducer_version = excluded.reducer_version, \
             last_seq = excluded.last_seq, \
             payload = excluded.payload",
    )
    .bind(key)
    .bind(i64::from(PROJECTION_REDUCER_VERSION))
    .bind(last_seq)
    .bind(payload)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Base for a fold inside a writer transaction: the in-process cache, else
/// the durable snapshot read through the same transaction.
pub(crate) async fn transaction_base(
    cache: &ProjectionCache,
    conn: &mut SqliteConnection,
    session: SessionId,
    expected_signature: &str,
) -> Result<Option<Arc<Projection>>, StoreError> {
    if let Some(base) = cache.get(session) {
        return Ok(Some(base.projection));
    }
    Ok(load_snapshot(conn, session, expected_signature)
        .await?
        .map(Arc::new))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use hya_proto::{Event, Projection, SessionId};

    use crate::{SessionStore, append_event_in_transaction, replay_projection};

    /// A fold inside a writer transaction starts from the cache and sees the
    /// transaction's own events, but a rolled-back event never reaches the
    /// cache that later reads start from.
    #[tokio::test]
    async fn transaction_fold_sees_own_events_without_poisoning_the_cache() {
        let store = SessionStore::connect_memory().await.unwrap();
        let session = SessionId::new();
        for title in ["one", "two", "three"] {
            store
                .append_event(
                    session,
                    &Event::SessionTitled {
                        session,
                        title: title.into(),
                    },
                )
                .await
                .unwrap();
        }
        let committed = store.read_projection(session).await.unwrap();
        assert!(
            store.projections.get(session).is_some(),
            "read warmed the cache"
        );

        let mut tx = store.begin_transaction().await.unwrap();
        append_event_in_transaction(
            &mut tx,
            session,
            Event::SessionTitled {
                session,
                title: "uncommitted".into(),
            },
        )
        .await
        .unwrap();
        let inside = replay_projection(&store.projections, &mut tx, session)
            .await
            .unwrap();
        assert_eq!(inside.session.title.as_deref(), Some("uncommitted"));
        tx.rollback().await.unwrap();

        assert_eq!(store.read_projection(session).await.unwrap(), committed);
        assert_eq!(
            store.read_projection(session).await.unwrap(),
            Projection::from_events(&store.replay(session).await.unwrap())
        );
    }

    #[tokio::test]
    async fn mark_persisted_never_reinserts_removed_or_stale_entry() {
        let store = SessionStore::connect_memory().await.unwrap();
        let session = SessionId::new();
        store
            .append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "one".into(),
                },
            )
            .await
            .unwrap();
        let proj = store.read_projection(session).await.unwrap();
        let last_seq = proj.last_seq;

        // Removing the entry (e.g. unconfirmed raw tx release) prevents mark_persisted from inserting.
        store.projections.remove_all([session]);
        store.projections.mark_persisted(session, last_seq);
        assert!(
            store.projections.get(session).is_none(),
            "mark_persisted must never re-insert a removed cache entry"
        );

        // Warm at last_seq: mismatching sequence leaves counters untouched; matching resets unpersisted.
        let _ = store.read_projection(session).await.unwrap();
        store.projections.mark_persisted(session, last_seq + 1);
        assert_eq!(store.projections.get(session).unwrap().persisted_seq, 0);
        store.projections.mark_persisted(session, last_seq);
        let updated = store.projections.get(session).unwrap();
        assert_eq!(updated.persisted_seq, last_seq);
        assert_eq!(updated.unpersisted, 0);
    }
}
