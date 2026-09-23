//! The projection cache (in-memory + durable snapshots) is a pure cache of the
//! shared reducer: every read equals a full replay of the event log, across
//! appends, restarts, deletes, forks, and reducer-version changes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../../hya-proto/tests/support/event_script.rs"]
mod event_script;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::{Event, PROJECTION_REDUCER_VERSION, Projection, SessionId};
use hya_store::SessionStore;
use uuid::Uuid;

struct TempDb(String);

impl TempDb {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        Self(
            std::env::temp_dir()
                .join(format!(
                    "hya-projection-cache-{nanos}-{}-{id}.db",
                    std::process::id()
                ))
                .to_string_lossy()
                .into_owned(),
        )
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", ".runtime-owner.lock"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0));
        }
    }
}

async fn full_replay(store: &SessionStore, session: SessionId) -> Projection {
    Projection::from_events(&store.replay(session).await.unwrap())
}

async fn raw_pool(path: &str) -> sqlx::SqlitePool {
    sqlx::SqlitePool::connect(&format!("sqlite://{path}"))
        .await
        .unwrap()
}

/// Property over generated logs: interleaved appends and reads through the
/// warm in-memory cache, and through a fresh store that starts from the
/// durable snapshot, always equal a full replay.
#[tokio::test]
async fn cached_reads_equal_full_replay_across_appends_and_restarts() {
    for seed in 0..16_u64 {
        let db = TempDb::new();
        let store = SessionStore::connect(&db.0)
            .await
            .unwrap()
            .with_projection_snapshot_interval(1 + seed % 7);
        let source = SessionId::from_uuid(Uuid::from_u128(u128::from(seed) + 1_000));
        let session = SessionId::from_uuid(Uuid::from_u128(u128::from(seed) + 2_000));
        let fork_of = (seed % 3 == 0).then_some(source);
        let events = event_script::script(seed, session, fork_of, 160);
        let mut rng = event_script::Rng::new(seed ^ 0xABCD);
        let mut next = 0;
        while next < events.len() {
            let chunk = 1 + rng.below(12);
            for event in events.iter().skip(next).take(chunk) {
                store.append_event(session, event).await.unwrap();
                // Unrelated traffic interleaves global sequence numbers.
                if rng.chance(20) {
                    store
                        .append_event(
                            source,
                            &Event::SessionTitled {
                                session: source,
                                title: "other".into(),
                            },
                        )
                        .await
                        .unwrap();
                }
            }
            next += chunk;
            let expected = full_replay(&store, session).await;
            assert_eq!(
                store.read_projection(session).await.unwrap(),
                expected,
                "warm read, seed {seed} after {next} events"
            );
            if rng.chance(35) {
                let restarted = SessionStore::connect(&db.0).await.unwrap();
                assert_eq!(
                    restarted.read_projection(session).await.unwrap(),
                    expected,
                    "restarted read, seed {seed} after {next} events"
                );
            }
        }
    }
}

/// The durable snapshot is what a fresh store starts from (so the tests above
/// are not vacuous), and it is ignored when another reducer version wrote it
/// or when its payload is not a snapshot.
#[tokio::test]
async fn durable_snapshot_is_used_only_under_the_current_reducer_version() {
    let db = TempDb::new();
    let store = SessionStore::connect(&db.0)
        .await
        .unwrap()
        .with_projection_snapshot_interval(1);
    let session = SessionId::new();
    for event in event_script::script(7, session, None, 60) {
        store.append_event(session, &event).await.unwrap();
    }
    let truth = store.read_projection(session).await.unwrap();

    // Poison the persisted snapshot: a fresh store must serve it verbatim.
    let mut poisoned = truth.clone();
    poisoned.session.title = Some("from the snapshot".into());
    let pool = raw_pool(&db.0).await;
    let updated = sqlx::query(
        "UPDATE projection_snapshot SET payload = ? WHERE session_id = ? AND reducer_version = ?",
    )
    .bind(poisoned.encode_snapshot().unwrap())
    .bind(session.storage_key())
    .bind(i64::from(PROJECTION_REDUCER_VERSION))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(updated.rows_affected(), 1, "the read persisted a snapshot");
    let fresh = SessionStore::connect(&db.0).await.unwrap();
    assert_eq!(
        fresh.read_projection(session).await.unwrap().session.title,
        Some("from the snapshot".into())
    );

    // Another reducer version: ignored, full replay wins.
    sqlx::query("UPDATE projection_snapshot SET reducer_version = ? WHERE session_id = ?")
        .bind(i64::from(PROJECTION_REDUCER_VERSION) + 1)
        .bind(session.storage_key())
        .execute(&pool)
        .await
        .unwrap();
    let fresh = SessionStore::connect(&db.0).await.unwrap();
    assert_eq!(fresh.read_projection(session).await.unwrap(), truth);

    // Current version but undecodable payload: ignored as well.
    sqlx::query(
        "UPDATE projection_snapshot SET reducer_version = ?, payload = ? WHERE session_id = ?",
    )
    .bind(i64::from(PROJECTION_REDUCER_VERSION))
    .bind(b"not a snapshot".to_vec())
    .bind(session.storage_key())
    .execute(&pool)
    .await
    .unwrap();
    let fresh = SessionStore::connect(&db.0).await.unwrap();
    assert_eq!(fresh.read_projection(session).await.unwrap(), truth);
    pool.close().await;
}

/// Deleting a session drops its cached and durable projection; the id reads
/// empty afterwards and a new log under the same id folds from scratch.
#[tokio::test]
async fn delete_session_invalidates_cached_projection() {
    let db = TempDb::new();
    let store = SessionStore::connect(&db.0)
        .await
        .unwrap()
        .with_projection_snapshot_interval(1);
    let session = SessionId::new();
    for event in event_script::script(3, session, None, 40) {
        store.append_event(session, &event).await.unwrap();
    }
    assert_ne!(
        store.read_projection(session).await.unwrap(),
        Projection::default()
    );

    assert!(store.delete_session(session).await.unwrap());
    assert_eq!(
        store.read_projection(session).await.unwrap(),
        Projection::default()
    );
    let pool = raw_pool(&db.0).await;
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projection_snapshot")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "delete removes the durable snapshot");
    pool.close().await;

    let reborn = event_script::script(4, session, None, 12);
    for event in &reborn {
        store.append_event(session, event).await.unwrap();
    }
    assert_eq!(
        store.read_projection(session).await.unwrap(),
        full_replay(&store, session).await
    );
}

/// A snapshot whose anchor event is gone (a log deleted or rewritten behind
/// this process, or a restored older database) is never trusted.
#[tokio::test]
async fn snapshot_without_its_anchor_event_is_ignored() {
    let db = TempDb::new();
    let store = SessionStore::connect(&db.0).await.unwrap();
    let session = SessionId::new();
    for event in event_script::script(5, session, None, 20) {
        store.append_event(session, &event).await.unwrap();
    }
    let truth = full_replay(&store, session).await;
    let mut stale = truth.clone();
    stale.session.title = Some("stale".into());
    stale.last_seq += 1_000;
    let pool = raw_pool(&db.0).await;
    sqlx::query(
        "INSERT OR REPLACE INTO projection_snapshot (session_id, reducer_version, last_seq, payload) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(session.storage_key())
    .bind(i64::from(PROJECTION_REDUCER_VERSION))
    .bind(i64::try_from(stale.last_seq).unwrap())
    .bind(stale.encode_snapshot().unwrap())
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let fresh = SessionStore::connect(&db.0).await.unwrap();
    assert_eq!(fresh.read_projection(session).await.unwrap(), truth);
}

/// `with_projection` reads through the same cache without cloning.
#[tokio::test]
async fn with_projection_sees_the_folded_projection() {
    let store = SessionStore::connect_memory().await.unwrap();
    let session = SessionId::new();
    for event in event_script::script(9, session, None, 50) {
        store.append_event(session, &event).await.unwrap();
    }
    let truth = full_replay(&store, session).await;
    let usage = store
        .with_projection(session, |projection| projection.session.usage.clone())
        .await
        .unwrap();
    assert_eq!(usage, truth.session.usage);
}

/// Bench + equivalence check on a real database (never modified: it is copied).
///
/// ```sh
/// HYA_PROJECTION_CACHE_DB=/path/to/run.db \
///   cargo test -p hya-store --test projection_cache -- --ignored --nocapture
/// ```
///
/// For every session: a cold read (full fold, snapshot written), a read from a
/// restarted store (durable snapshot + tail), and a warm read must all equal
/// `Projection::from_events(replay)`; prints the time of each.
#[tokio::test]
#[ignore = "needs HYA_PROJECTION_CACHE_DB pointing at a real database"]
async fn real_database_cached_reads_equal_full_replay() {
    use std::time::Instant;

    let Ok(source) = std::env::var("HYA_PROJECTION_CACHE_DB") else {
        eprintln!("HYA_PROJECTION_CACHE_DB unset; nothing to check");
        return;
    };
    let db = TempDb::new();
    std::fs::copy(&source, &db.0).unwrap();
    if std::path::Path::new(&format!("{source}-wal")).is_file() {
        std::fs::copy(format!("{source}-wal"), format!("{}-wal", db.0)).unwrap();
    }
    let store = SessionStore::connect(&db.0).await.unwrap();
    let sessions = store.list_sessions().await.unwrap();
    let mut totals = [0_u128; 4];
    for row in &sessions {
        let started = Instant::now();
        let truth = full_replay(&store, row.session).await;
        let replay_ms = started.elapsed().as_millis();

        let started = Instant::now();
        let cold = store.read_projection(row.session).await.unwrap();
        let cold_ms = started.elapsed().as_millis();

        let restarted = SessionStore::connect(&db.0).await.unwrap();
        let started = Instant::now();
        let from_snapshot = restarted.read_projection(row.session).await.unwrap();
        let snapshot_ms = started.elapsed().as_millis();

        let started = Instant::now();
        let warm = restarted.read_projection_shared(row.session).await.unwrap();
        let warm_ms = started.elapsed().as_millis();

        assert_eq!(cold, truth, "cold read of {}", row.session);
        assert_eq!(from_snapshot, truth, "snapshot read of {}", row.session);
        assert_eq!(*warm, truth, "warm read of {}", row.session);
        eprintln!(
            "{} events={} full_replay={replay_ms}ms cold={cold_ms}ms snapshot={snapshot_ms}ms warm={warm_ms}ms",
            row.session, row.events
        );
        for (total, ms) in totals
            .iter_mut()
            .zip([replay_ms, cold_ms, snapshot_ms, warm_ms])
        {
            *total += ms;
        }
    }
    eprintln!(
        "sessions={} full_replay={}ms cold={}ms snapshot={}ms warm={}ms",
        sessions.len(),
        totals[0],
        totals[1],
        totals[2],
        totals[3]
    );
}
