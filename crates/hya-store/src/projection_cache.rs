//! Replay-consistent projection cache.
//!
//! `read_projection` used to decode and fold a session's whole event log on
//! every call. This module keeps the shared reducer's output cached at two
//! levels, both pure caches of the event log (never a source of truth):
//!
//! - an in-process map `session -> Arc<Projection>` shared by every clone of a
//!   [`crate::SessionStore`], advanced on read by folding only the events
//!   after the cached projection's `last_seq`;
//! - durable rows in `projection_snapshot` (migration 0011), written every
//!   [`DEFAULT_SNAPSHOT_INTERVAL`] newly folded events, so a restarted process
//!   folds only the tail instead of the whole log.
//!
//! Invariants that make "cached base + tail" equal a full replay:
//!
//! - `event_log` is append-only per session (the only removal is
//!   `delete_session`, which drops every row of the session) and `seq` is a
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
use std::sync::{Arc, Mutex};

use futures::TryStreamExt as _;
use hya_proto::{Envelope, Event, EventSeq, PROJECTION_REDUCER_VERSION, Projection, SessionId};
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
            clock: AtomicU64::new(0),
            snapshot_interval: AtomicU64::new(DEFAULT_SNAPSHOT_INTERVAL),
        }
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

    /// Record a fold; an older fold never replaces a newer one.
    pub(crate) fn put(
        &self,
        session: SessionId,
        projection: Arc<Projection>,
        persisted_seq: u64,
        unpersisted: u64,
    ) {
        let now = self.tick();
        let mut entries = self.entries();
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

    pub(crate) fn remove(&self, session: SessionId) {
        self.entries().remove(&session);
    }
}

fn seq_param(seq: u64) -> i64 {
    i64::try_from(seq).unwrap_or(i64::MAX)
}

fn row_seq(row: &SqliteRow) -> Result<u64, StoreError> {
    let seq: i64 = row.try_get("seq")?;
    Ok(seq.max(0) as u64)
}

fn decode_row(row: &SqliteRow) -> Result<Envelope, StoreError> {
    let ts_millis: i64 = row.try_get("ts")?;
    let payload: &str = row.try_get("payload")?;
    let event: Event = serde_json::from_str(payload)?;
    Ok(Envelope {
        seq: EventSeq(row_seq(row)?),
        ts_millis,
        event,
    })
}

/// The durable snapshot of `session`, when one exists under the running
/// reducer version and decodes.
pub(crate) async fn load_snapshot(
    conn: &mut SqliteConnection,
    session: SessionId,
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
    match Projection::decode_snapshot(payload) {
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
) -> Result<Folded, StoreError> {
    let key = session.storage_key();
    if let Some(base) = base.filter(|base| base.last_seq > 0) {
        let from = base.last_seq;
        let mut anchored = false;
        let mut next: Option<Projection> = None;
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
                let envelope = decode_row(&row)?;
                next.get_or_insert_with(|| Projection::clone(&base))
                    .apply(&envelope);
                applied += 1;
            }
        }
        if anchored {
            return Ok(Folded {
                projection: next.map_or(base, Arc::new),
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
            projection.apply(&decode_row(&row)?);
            applied += 1;
        }
    }
    Ok(Folded {
        projection: Arc::new(projection),
        applied,
        rebuilt: true,
    })
}

/// Upsert the durable snapshot of `projection`.
///
/// Written only while the anchor event still exists, so a snapshot racing a
/// `delete_session` is a no-op. Replacing a newer row with an older (still
/// anchored) fold is harmless: any anchored prefix is a valid base.
pub(crate) async fn persist_snapshot(
    conn: &mut SqliteConnection,
    session: SessionId,
    projection: &Projection,
) -> Result<(), StoreError> {
    let payload = projection.encode_snapshot()?;
    let key = session.storage_key();
    let last_seq = seq_param(projection.last_seq);
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
) -> Result<Option<Arc<Projection>>, StoreError> {
    if let Some(base) = cache.get(session) {
        return Ok(Some(base.projection));
    }
    Ok(load_snapshot(conn, session).await?.map(Arc::new))
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

        let mut tx = store.pool.begin().await.unwrap();
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
}
