//! Fault-injection integration tests for [`hya_store`].
//!
//! This file is compiled as its own integration test binary separate from
//! `tests/it` because:
//! 1. It registers a custom SQLite VFS as the process-global default
//!    (`sqlite3_vfs_register(..., 1)`), since `SessionStore::connect` configures
//!    its own connection options.
//! 2. On Linux it interposes `clock_gettime` process-wide so pool lifetime and
//!    idle-timeout tests do not need to sleep 30+ minutes.
//!
//! Neither global hook may leak into `tests/it`.
//!
//! # Monotonic clock shim contract
//!
//! On Linux, [`clock_gettime`] adds a monotonically non-decreasing offset to
//! `CLOCK_MONOTONIC` and `CLOCK_MONOTONIC_RAW` for every caller in this binary
//! (bypassing vDSO for clock reads). Because the test harness (`libtest`) reads
//! the same monotonic clock, its printed summary reflects the simulated jump
//! (for example `finished in 1830.16s` and a `has been running for over 60
//! seconds` warning) even though actual wall-clock runtime is sub-second.
//!
//! **Every test in this file MUST hold [`clock_gate()`]** — `.write().await`
//! when advancing the clock, or `.read().await` otherwise — so no test advances
//! time concurrently under another test.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_arguments)]

use hya_proto::{
    AgentName, Envelope, Event, EventSeq, FinishReason, MessageId, ModelRef, PartId, Projection,
    Role, SessionId, SessionKind,
};
use hya_store::{ExitDrainReport, SavedPermission, SessionStore, StoreError, StoreHealth};
#[cfg(target_os = "linux")]
use libsqlite3_sys::SQLITE_BUSY;
use libsqlite3_sys::SQLITE_LOCK_EXCLUSIVE;
use sqlx::Connection as _;
use sqlx::sqlite::SqliteConnectOptions;

include!("support/fault_vfs.rs");

impl FaultPolicy {
    fn wait_for_held_wal_write(&self, min_held: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut gate = self
            .write_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while gate.held_writes < min_held {
            let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (next, res) = self
                .write_entered_cv
                .wait_timeout(gate, rem)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            gate = next;
            if res.timed_out() && gate.held_writes < min_held {
                return false;
            }
        }
        true
    }
}

struct NotifyWaker(Arc<tokio::sync::Notify>);

impl std::task::Wake for NotifyWaker {
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify_one();
    }
}

struct RestoreMode(PathBuf);

impl Drop for RestoreMode {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
    }
}

async fn wait_until_healthy(store: &SessionStore, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if store.health() == StoreHealth::Healthy {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "store stayed degraded past {timeout:?}: {:?}",
            store.health()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn session_created_event(session: SessionId, workdir: &Path) -> Event {
    Event::SessionCreated {
        session,
        parent: None,
        agent: AgentName::new("build"),
        model: ModelRef::new("fake"),
        workdir: workdir.to_string_lossy().into_owned(),
        project: None,
        kind: SessionKind::Project,
    }
}

fn message_events(session: SessionId, text: &str) -> Vec<Event> {
    let message = MessageId::new();
    let part = PartId::new();
    vec![
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
            agent: None,
            model: None,
        },
        Event::TextStart {
            session,
            message,
            part,
        },
        Event::TextDelta {
            session,
            message,
            part,
            delta: text.to_string(),
        },
        Event::TextEnd {
            session,
            message,
            part,
        },
        Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
            cause: None,
        },
    ]
}

async fn assert_parity_with_healthy_store(
    actual_store: &SessionStore,
    healthy_ref: &SessionStore,
    session: SessionId,
    actual_envelopes: &[Envelope],
    expected_envelopes: &[Envelope],
) {
    let actual_proj = actual_store.read_projection(session).await.unwrap();
    let healthy_proj = healthy_ref.read_projection(session).await.unwrap();
    assert_eq!(actual_proj, Projection::from_events(actual_envelopes));
    assert_eq!(actual_proj.last_seq, healthy_proj.last_seq);
    assert_eq!(actual_proj.session.id, healthy_proj.session.id);
    assert_eq!(actual_proj.session.title, healthy_proj.session.title);
    assert_eq!(actual_proj.session.agent, healthy_proj.session.agent);
    assert_eq!(actual_proj.extensions, healthy_proj.extensions);
    assert_eq!(
        actual_proj.session.messages.len(),
        healthy_proj.session.messages.len()
    );
    fn seqs_and_events(envs: &[Envelope]) -> Vec<(EventSeq, &Event)> {
        envs.iter().map(|e| (e.seq, &e.event)).collect()
    }
    assert_eq!(
        seqs_and_events(actual_envelopes),
        seqs_and_events(expected_envelopes)
    );
}

fn poll_pending<F: std::future::Future + Unpin>(fut: &mut F) -> bool {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    std::pin::Pin::new(fut).poll(&mut cx).is_pending()
}

fn count_wal_write_locks(trace: &[VfsEvent]) -> usize {
    trace
        .iter()
        .filter(|e| {
            e.kind == FileKind::MainDb
                && e.op == "xShmLock"
                && e.offset == 0
                && e.arg == i64::from(SQLITE_SHM_LOCK_EXCL)
                && e.rc == SQLITE_OK
        })
        .count()
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn session_store_does_not_retire_connection_after_max_lifetime_or_idle() {
    let _gate = clock_gate().write().await;
    let case = FaultCase::new("no-max-lifetime-retire");
    let db = case.db_path("hya.db");
    let store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
    let session = SessionId::new();
    store
        .append_events(
            session,
            vec![Event::SessionArchived {
                session,
                archived: 1.into(),
            }],
        )
        .await
        .unwrap();

    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    let _ = ro.session_exists(session).await.unwrap();

    let mut tx0 = store.begin_transaction().await.unwrap();
    advance_monotonic_secs(1830);
    case.policy.take_trace();

    hya_store::append_event_in_transaction(
        &mut tx0,
        session,
        Event::SessionTitled {
            session,
            title: "before-drop".into(),
        },
    )
    .await
    .unwrap();
    tx0.commit().await.unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;

    store
        .append_events(
            session,
            vec![Event::SessionTitled {
                session,
                title: "after-drop".into(),
            }],
        )
        .await
        .expect("append after 1830s jump should succeed");
    tokio::time::sleep(Duration::from_millis(15)).await;

    let trace = case.policy.take_trace();
    let closed = trace
        .iter()
        .any(|e| e.kind == FileKind::MainDb && e.op == "xClose");
    let locked_excl = trace.iter().any(|e| {
        e.kind == FileKind::MainDb && e.op == "xLock" && e.arg == i64::from(SQLITE_LOCK_EXCLUSIVE)
    });
    let hit_busy = trace
        .iter()
        .any(|e| e.kind == FileKind::MainDb && e.op == "xLock" && e.rc == SQLITE_BUSY);
    assert!(
        !closed && !locked_excl && !hit_busy,
        "unexpected WAL close/retirement after 1830s jump: {trace:?}"
    );

    case.policy.take_trace();
    let _ = ro.session_exists(session).await.unwrap();
    tokio::time::sleep(Duration::from_millis(15)).await;
    let trace_ro = case.policy.take_trace();
    assert!(
        !trace_ro
            .iter()
            .any(|e| (e.kind == FileKind::MainDb && e.op == "xClose")
                || (e.kind == FileKind::Wal && e.op == "xDelete")),
        "read-only pool retired a connection: {trace_ro:?}"
    );
}

#[tokio::test]
async fn single_writer_pool_recovers_after_enospc_auto_rollback() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("writer-enospc-recover");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_events(
            session,
            vec![Event::SessionArchived {
                session,
                archived: 1.into(),
            }],
        )
        .await
        .unwrap();

    case.policy.enospc.store(true, Ordering::Relaxed);
    let queued = store
        .append_events(
            session,
            vec![Event::SessionTitled {
                session,
                title: "x".repeat(32 * 1024),
            }],
        )
        .await
        .expect("append under ENOSPC must degrade and return queued envelope");
    assert_eq!(queued[0].seq, EventSeq(2));
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    case.policy.enospc.store(false, Ordering::Relaxed);
    let third = store
        .append_events(
            session,
            vec![Event::SessionTitled {
                session,
                title: "after-enospc".into(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(third[0].seq, EventSeq(3));

    wait_until_healthy(&store, Duration::from_secs(5)).await;
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    let seqs: Vec<u64> = ro
        .replay(session)
        .await
        .unwrap()
        .iter()
        .map(|e| e.seq.0)
        .collect();
    assert_eq!(seqs, vec![1, 2, 3]);
    assert_eq!(
        ro.read_projection(session)
            .await
            .unwrap()
            .session
            .title
            .as_deref(),
        Some("after-enospc")
    );
}

#[tokio::test]
async fn cancelled_writer_acquire_does_not_close_pinned_writer() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("writer-cancel-no-close");
    let db = case.db_path("hya.db");
    let store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
    let session = SessionId::new();
    store
        .append_events(
            session,
            vec![Event::SessionArchived {
                session,
                archived: 1.into(),
            }],
        )
        .await
        .unwrap();

    let tx = store.begin_transaction().await.unwrap();
    let wake = Arc::new(tokio::sync::Notify::new());
    let waker = std::task::Waker::from(Arc::new(NotifyWaker(Arc::clone(&wake))));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut fut = Box::pin(store.begin_transaction());
    assert!(std::future::Future::poll(fut.as_mut(), &mut cx).is_pending());

    let _ = case.policy.take_trace();
    drop(tx);
    tokio::time::timeout(Duration::from_secs(5), wake.notified())
        .await
        .expect("writer return must wake pending acquire waiter");
    let _ = std::future::Future::poll(fut.as_mut(), &mut cx);
    drop(fut);

    store
        .append_events(
            session,
            vec![Event::SessionTitled {
                session,
                title: "after-cancel".into(),
            }],
        )
        .await
        .unwrap();

    let trace = case.policy.take_trace();
    assert!(
        !trace.iter().any(|e| {
            (e.kind == FileKind::MainDb
                && (e.op == "xOpen"
                    || e.op == "xClose"
                    || (e.op == "xLock" && e.arg == i64::from(SQLITE_LOCK_EXCLUSIVE))))
                || (e.kind == FileKind::Wal && e.op == "xClose")
        }),
        "cancelled acquire disturbed pinned writer: {trace:?}"
    );
    assert_eq!(store.replay(session).await.unwrap().len(), 2);
}

#[tokio::test]
async fn write_stall_degrades_preserves_projection_parity_session_exists_and_exact_disk_seqs() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("write-stall");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    let healthy = SessionStore::connect_memory().await.unwrap();

    // Session A: warm in cache with an extra healthy commit before degrade (advances warm cache).
    // Session B: created entirely during the write stall (tests degraded session_exists & parity).
    let session_a = SessionId::new();
    let session_b = SessionId::new();
    let created_a = session_created_event(session_a, &case.dir);
    let mut all_a = store
        .append_events(session_a, vec![created_a.clone()])
        .await
        .unwrap();
    let mut ref_a = healthy
        .append_events(session_a, vec![created_a])
        .await
        .unwrap();
    let _ = store.read_projection(session_a).await.unwrap();

    let warmed = Event::SessionTitled {
        session: session_a,
        title: "healthy-warmed".into(),
    };
    all_a.extend(
        store
            .append_events(session_a, vec![warmed.clone()])
            .await
            .unwrap(),
    );
    ref_a.extend(
        healthy
            .append_events(session_a, vec![warmed])
            .await
            .unwrap(),
    );
    assert_eq!(store.health(), StoreHealth::Healthy);

    case.policy.hold_wal_writes(true);
    let mut batch_b = vec![session_created_event(session_b, &case.dir)];
    batch_b.extend(message_events(session_b, "new session during stall"));
    let all_b = store
        .append_events(session_b, batch_b.clone())
        .await
        .unwrap();
    let ref_b = healthy.append_events(session_b, batch_b).await.unwrap();
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    // Warm session A reflects its pre-degrade healthy commit while degraded without queued ops.
    assert_eq!(
        store
            .read_projection(session_a)
            .await
            .unwrap()
            .session
            .title
            .as_deref(),
        Some("healthy-warmed")
    );
    // Degraded session_exists sees queued-only session B, not absent ids.
    assert!(store.session_exists(session_b).await.unwrap());
    assert!(!store.session_exists(SessionId::new()).await.unwrap());

    let title_a = Event::SessionTitled {
        session: session_a,
        title: "degraded-title".into(),
    };
    let mut extra_a = vec![title_a];
    extra_a.extend(message_events(session_a, "reply during stall"));
    all_a.extend(
        store
            .append_events(session_a, extra_a.clone())
            .await
            .unwrap(),
    );
    ref_a.extend(healthy.append_events(session_a, extra_a).await.unwrap());

    assert_parity_with_healthy_store(&store, &healthy, session_a, &all_a, &ref_a).await;
    assert_parity_with_healthy_store(&store, &healthy, session_b, &all_b, &ref_b).await;
    assert_eq!(store.replay(session_a).await.unwrap(), all_a);
    assert_eq!(store.replay(session_b).await.unwrap(), all_b);

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;

    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.replay(session_a).await.unwrap(), all_a);
    assert_eq!(ro.replay(session_b).await.unwrap(), all_b);
    assert_eq!(
        ro.read_projection(session_a).await.unwrap(),
        Projection::from_events(&all_a)
    );
    assert_eq!(
        ro.read_projection(session_b).await.unwrap(),
        Projection::from_events(&all_b)
    );
}

#[tokio::test]
async fn late_committing_write_lands_once_with_assigned_seq() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("late-commit");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    let session = SessionId::new();
    let e1 = session_created_event(session, &case.dir);
    let (seq1, ts1) = store.append_event(session, &e1).await.unwrap();
    assert_eq!(seq1, EventSeq(1));

    case.policy.hold_wal_writes(true);
    let e2 = Event::SessionTitled {
        session,
        title: "late-commit".into(),
    };
    let (seq2, ts2) = store.append_event(session, &e2).await.unwrap();
    assert_eq!(seq2, EventSeq(2));
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
    assert!(
        case.policy
            .wait_for_held_wal_write(1, Duration::from_secs(2))
    );

    let e3 = Event::SessionTitled {
        session,
        title: "after-late-commit".into(),
    };
    let (seq3, ts3) = store.append_event(session, &e3).await.unwrap();
    assert_eq!(seq3, EventSeq(3));

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;

    let expected = vec![
        Envelope {
            seq: seq1,
            ts_millis: ts1,
            event: e1,
        },
        Envelope {
            seq: seq2,
            ts_millis: ts2,
            event: e2,
        },
        Envelope {
            seq: seq3,
            ts_millis: ts3,
            event: e3,
        },
    ];
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.replay(session).await.unwrap(), expected);
}

#[tokio::test]
async fn existing_history_preserves_seq_and_projection_parity_when_degraded() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("existing-history");
    let db = case.db_path("hya.db");
    let session = SessionId::new();
    let healthy = SessionStore::connect_memory().await.unwrap();

    const PRIOR_COUNT: u64 = 300;
    let mut initial = vec![session_created_event(session, &case.dir)];
    for i in 1..PRIOR_COUNT {
        initial.push(Event::SessionTitled {
            session,
            title: format!("hist-{i}"),
        });
    }
    let mut ref_envelopes = healthy
        .append_events(session, initial.clone())
        .await
        .unwrap();
    let mut disk_envelopes = {
        let prior = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        prior.append_events(session, initial).await.unwrap()
    };
    assert_eq!(disk_envelopes.len(), PRIOR_COUNT as usize);

    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    case.policy.hold_wal_writes(true);

    for offset in 1..=3_u64 {
        let ev = Event::SessionTitled {
            session,
            title: format!("degraded-{}", PRIOR_COUNT + offset),
        };
        let (seq, ts) = store.append_event(session, &ev).await.unwrap();
        assert_eq!(seq, EventSeq(PRIOR_COUNT + offset));
        assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
        disk_envelopes.push(Envelope {
            seq,
            ts_millis: ts,
            event: ev.clone(),
        });
        ref_envelopes.extend(healthy.append_events(session, vec![ev]).await.unwrap());
        assert_parity_with_healthy_store(
            &store,
            &healthy,
            session,
            &disk_envelopes,
            &ref_envelopes,
        )
        .await;
    }

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.replay(session).await.unwrap(), disk_envelopes);
    assert_eq!(
        ro.read_projection(session).await.unwrap(),
        Projection::from_events(&disk_envelopes)
    );
}

#[tokio::test]
async fn external_write_lock_degrades_and_flushes_on_release() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("external-lock");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    let session = SessionId::new();
    let e1 = session_created_event(session, &case.dir);
    let (seq1, ts1) = store.append_event(session, &e1).await.unwrap();
    assert_eq!(seq1, EventSeq(1));

    let ext_opts = SqliteConnectOptions::new()
        .filename(&db)
        .busy_timeout(Duration::from_millis(50));
    let mut ext = sqlx::SqliteConnection::connect_with(&ext_opts)
        .await
        .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut ext)
        .await
        .unwrap();

    let e2 = Event::SessionTitled {
        session,
        title: "under-external-lock".into(),
    };
    let e3 = Event::SessionArchived {
        session,
        archived: 2.into(),
    };
    let (seq2, ts2) = store.append_event(session, &e2).await.unwrap();
    let (seq3, ts3) = store.append_event(session, &e3).await.unwrap();
    assert_eq!((seq2, seq3), (EventSeq(2), EventSeq(3)));
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    let expected = vec![
        Envelope {
            seq: seq1,
            ts_millis: ts1,
            event: e1,
        },
        Envelope {
            seq: seq2,
            ts_millis: ts2,
            event: e2,
        },
        Envelope {
            seq: seq3,
            ts_millis: ts3,
            event: e3,
        },
    ];
    assert_eq!(
        store.read_projection(session).await.unwrap(),
        Projection::from_events(&expected)
    );

    sqlx::query("ROLLBACK").execute(&mut ext).await.unwrap();
    drop(ext);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.replay(session).await.unwrap(), expected);
}

#[tokio::test]
async fn logical_error_returns_to_caller_without_degrading() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("logical-error");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();

    // 1. Pre-queue logical validation error.
    let ext_err = store
        .append_event(
            session,
            &Event::Extension {
                session,
                bundle: String::new(),
                kind: "msg".into(),
                payload: serde_json::json!({}),
            },
        )
        .await
        .unwrap_err();
    assert!(!ext_err.to_string().is_empty());
    assert_eq!(store.health(), StoreHealth::Healthy);

    // 2. SQLite-level logical abort inside the single writer task.
    let mut tx = store.begin_transaction().await.unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_forbidden BEFORE INSERT ON event_log \
         WHEN NEW.payload LIKE '%forbidden-title%' \
         BEGIN SELECT RAISE(ABORT, 'injected logical error'); END",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let trig_err = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "forbidden-title".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(trig_err.to_string().contains("injected logical error"));
    assert_eq!(store.health(), StoreHealth::Healthy);

    let mut tx = store.begin_transaction().await.unwrap();
    sqlx::query("DROP TRIGGER reject_forbidden")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "healthy-after-logical".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(store.health(), StoreHealth::Healthy);
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        ro.read_projection(session)
            .await
            .unwrap()
            .session
            .title
            .as_deref(),
        Some("healthy-after-logical")
    );
}

#[tokio::test]
async fn non_append_while_degraded_refuses_immediately_and_preserves_queue() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("non-append-degraded");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(80))
            .await
            .unwrap();
    let session = SessionId::new();
    let e1 = session_created_event(session, &case.dir);
    let (seq1, ts1) = store.append_event(session, &e1).await.unwrap();

    case.policy.hold_wal_writes(true);
    let e2 = Event::SessionTitled {
        session,
        title: "queued-while-stalled".into(),
    };
    let (seq2, ts2) = store.append_event(session, &e2).await.unwrap();
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    // Non-append mutations refuse immediately (< write_timeout) without blocking appends.
    let t0 = Instant::now();
    assert!(matches!(
        store.begin_transaction().await,
        Err(StoreError::StoreDegraded { .. })
    ));
    assert!(
        t0.elapsed() < Duration::from_millis(60),
        "degraded begin_transaction waited: {:?}",
        t0.elapsed()
    );
    assert!(matches!(
        store
            .save_permission(&SavedPermission {
                id: "perm-1".into(),
                project_id: "global".into(),
                action: "read".into(),
                resource: "/tmp".into(),
                time_created_ms: Some(1),
            })
            .await,
        Err(StoreError::StoreDegraded { .. })
    ));
    assert!(matches!(
        store.delete_session(session).await,
        Err(StoreError::StoreDegraded { .. })
    ));

    let t1 = Instant::now();
    let e3 = Event::SessionTitled {
        session,
        title: "queued-final".into(),
    };
    let (seq3, ts3) = store.append_event(session, &e3).await.unwrap();
    assert_eq!((seq1, seq2, seq3), (EventSeq(1), EventSeq(2), EventSeq(3)));
    assert!(t1.elapsed() < Duration::from_millis(60));

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;

    let expected = vec![
        Envelope {
            seq: seq1,
            ts_millis: ts1,
            event: e1,
        },
        Envelope {
            seq: seq2,
            ts_millis: ts2,
            event: e2,
        },
        Envelope {
            seq: seq3,
            ts_millis: ts3,
            event: e3,
        },
    ];
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.replay(session).await.unwrap(), expected);
    assert!(ro.list_saved_permissions(None).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn healthy_cold_read_and_anchored_followup_never_cache_incomplete_fold() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("healthy-cold-read");
    let db = case.db_path("hya.db");
    let other = SessionId::new();
    let cold = SessionId::new();
    {
        let prior = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        for (s, ch) in [(other, "a"), (cold, "b")] {
            let mut events = vec![session_created_event(s, &case.dir)];
            events.extend(message_events(s, &ch.repeat(16_384)));
            prior.append_events(s, events).await.unwrap();
        }
    }

    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
            .await
            .unwrap();
    let _ = store.read_projection(other).await.unwrap();

    // Part A: deterministic VFS xRead race on first cold fold -> anchored follow-up picks up commit and warms cache.
    let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let st = store.clone();
    let racer = tokio::spawn(async move {
        if trig_rx.await.is_err() {
            return;
        }
        st.append_event(
            cold,
            &Event::SessionTitled {
                session: cold,
                title: "raced-e2".into(),
            },
        )
        .await
        .unwrap();
        let _ = done_tx.send(());
    });
    case.policy.arm_shm_read_hook(2, move || {
        let _ = trig_tx.send(());
        let _ = done_rx.recv();
    });
    let r1 = store.read_projection(cold).await.unwrap();
    assert!(!case.policy.clear_shm_read_hook());
    racer.await.unwrap();
    assert_eq!(r1.session.title.as_deref(), Some("raced-e2"));

    // Prove anchored follow-up warmed the cache: degraded read under fail_main_reads touches zero disk pages.
    case.policy.hold_wal_writes(true);
    store
        .append_event(
            cold,
            &Event::SessionArchived {
                session: cold,
                archived: 3.into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
    case.policy.fail_main_reads.store(true, Ordering::Relaxed);
    let _ = case.policy.take_trace();
    let degraded = store.read_projection(cold).await.unwrap();
    let trace = case.policy.take_trace();
    case.policy.fail_main_reads.store(false, Ordering::Relaxed);
    assert_eq!(degraded.session.title.as_deref(), Some("raced-e2"));
    assert_eq!(degraded.session.archived, Some(3.into()));
    assert!(!trace.iter().any(|ev| ev.op == "xRead"));

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;

    // Part B: steady concurrent same-session + cross-session writes preserve exact prefix parity
    // and converge to cached after writes stop.
    let steady = SessionId::new();
    let mut initial = vec![session_created_event(steady, &case.dir)];
    for i in 0..40 {
        initial.extend(message_events(steady, &format!("h{i}")));
    }
    store.append_events(steady, initial).await.unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let same_w = store.clone();
    let same_stop = Arc::clone(&stop);
    let same_task = tokio::spawn(async move {
        let mut k = 0_u64;
        while !same_stop.load(Ordering::Relaxed) {
            same_w
                .append_event(
                    steady,
                    &Event::SessionTitled {
                        session: steady,
                        title: format!("steady-{k}"),
                    },
                )
                .await
                .unwrap();
            k += 1;
            tokio::task::yield_now().await;
        }
        k
    });
    let side_w = store.clone();
    let side_stop = Arc::clone(&stop);
    let side_task = tokio::spawn(async move {
        let side = SessionId::new();
        side_w
            .append_event(side, &session_created_event(side, &PathBuf::from("/tmp")))
            .await
            .unwrap();
        let mut k = 0_u64;
        while !side_stop.load(Ordering::Relaxed) && k < 10 {
            side_w
                .append_event(
                    side,
                    &Event::SessionTitled {
                        session: side,
                        title: format!("side-{k}"),
                    },
                )
                .await
                .unwrap();
            k += 1;
        }
    });

    for _ in 0..10 {
        let proj = store.read_projection(steady).await.unwrap();
        let disk = store.replay(steady).await.unwrap();
        let prefix: Vec<_> = disk
            .into_iter()
            .filter(|e| e.seq.0 <= proj.last_seq)
            .collect();
        assert_eq!(proj, Projection::from_events(&prefix));
    }
    stop.store(true, Ordering::Relaxed);
    assert!(same_task.await.unwrap() > 0);
    side_task.await.unwrap();

    let after_stop = store.read_projection(steady).await.unwrap();
    case.policy.hold_wal_writes(true);
    let deg = SessionId::new();
    store
        .append_event(deg, &session_created_event(deg, &case.dir))
        .await
        .unwrap();
    case.policy.fail_main_reads.store(true, Ordering::Relaxed);
    let _ = case.policy.take_trace();
    let cached = store.read_projection(steady).await.unwrap();
    let trace_after = case.policy.take_trace();
    case.policy.fail_main_reads.store(false, Ordering::Relaxed);
    assert_eq!(cached, after_stop);
    assert!(!trace_after.iter().any(|ev| ev.op == "xRead"));
    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
}

#[tokio::test]
async fn cold_read_during_catchup_and_straddling_degradation_preserve_parity() {
    let _gate = clock_gate().read().await;

    // Part A: cold read during catch-up succeeds with full parity.
    {
        let case = FaultCase::new("cold-read-catchup");
        let db = case.db_path("hya.db");
        let other = SessionId::new();
        let session = SessionId::new();
        {
            let prior = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
            for (s, ch) in [(other, "c"), (session, "d")] {
                let mut events = vec![session_created_event(s, &case.dir)];
                events.extend(message_events(s, &ch.repeat(16_384)));
                prior.append_events(s, events).await.unwrap();
            }
        }
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        let _ = store.read_projection(other).await.unwrap();

        case.policy.hold_wal_writes(true);
        store
            .append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "catchup-e2".into(),
                },
            )
            .await
            .unwrap();
        store
            .append_event(
                session,
                &Event::SessionArchived {
                    session,
                    archived: 2.into(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
        assert!(
            case.policy
                .wait_for_held_wal_write(1, Duration::from_secs(2))
        );

        let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let pol = Arc::clone(&case.policy);
        let st = store.clone();
        let flusher = tokio::spawn(async move {
            if trig_rx.await.is_err() {
                return;
            }
            pol.hold_wal_writes(false);
            wait_until_healthy(&st, Duration::from_secs(5)).await;
            let _ = done_tx.send(());
        });
        case.policy.arm_shm_read_hook(2, move || {
            let _ = trig_tx.send(());
            let _ = done_rx.recv();
        });
        let proj = store.read_projection(session).await.unwrap();
        assert!(!case.policy.clear_shm_read_hook());
        flusher.await.unwrap();
        assert_eq!(store.health(), StoreHealth::Healthy);
        assert_eq!(proj.session.title.as_deref(), Some("catchup-e2"));
        assert_eq!(proj.session.archived, Some(2.into()));
    }

    // Part B: cold fold straddling degradation + halted flush never caches missed commit.
    {
        let case = FaultCase::new("cold-fold-straddling-degradation");
        let db = case.db_path("hya.db");
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(60),
        )
        .await
        .unwrap();
        let mut tx = store.begin_transaction().await.unwrap();
        sqlx::query(
            "CREATE TRIGGER fold_cover_halt BEFORE INSERT ON event_log \
             WHEN NEW.payload LIKE '%halt-here%' \
             BEGIN SELECT RAISE(ABORT, 'fold cover halt'); END",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let other = SessionId::new();
        let session = SessionId::new();
        for (s, ch) in [(other, "a"), (session, "b")] {
            let mut events = vec![session_created_event(s, &case.dir)];
            events.extend(message_events(s, &ch.repeat(16_384)));
            store.append_events(s, events).await.unwrap();
        }
        let _ = store.read_projection(other).await.unwrap();

        let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let st = store.clone();
        let pol = Arc::clone(&case.policy);
        let dbp = db.clone();
        let racer = tokio::spawn(async move {
            if trig_rx.await.is_err() {
                return;
            }
            pol.hold_wal_writes(true);
            let c_store = st.clone();
            let c = tokio::spawn(async move {
                c_store
                    .append_event(
                        session,
                        &Event::SessionTitled {
                            session,
                            title: "c-committed".into(),
                        },
                    )
                    .await
            });
            let pol2 = Arc::clone(&pol);
            assert!(
                tokio::task::spawn_blocking(move || {
                    pol2.wait_for_held_wal_write(1, Duration::from_secs(2))
                })
                .await
                .unwrap()
            );
            c.await.unwrap().unwrap();
            assert!(matches!(st.health(), StoreHealth::Degraded { .. }));
            st.append_event(
                session,
                &Event::SessionArchived {
                    session,
                    archived: 9.into(),
                },
            )
            .await
            .unwrap();
            st.append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "halt-here".into(),
                },
            )
            .await
            .unwrap();
            pol.hold_wal_writes(false);

            let ro = SessionStore::connect_read_only(dbp.to_str().unwrap())
                .await
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if ro
                    .replay(session)
                    .await
                    .unwrap()
                    .into_iter()
                    .any(|e| matches!(e.event, Event::SessionArchived { .. }))
                {
                    break;
                }
                assert!(Instant::now() < deadline, "archived never flushed");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let _ = done_tx.send(());
        });

        case.policy.arm_shm_read_hook(2, move || {
            let _ = trig_tx.send(());
            let _ = done_rx.recv();
        });
        let r1 = store.read_projection(session).await.unwrap();
        assert!(!case.policy.clear_shm_read_hook());
        racer.await.unwrap();
        assert_eq!(r1.session.archived, Some(9.into()));
        assert_eq!(
            store.read_projection(session).await.unwrap(),
            Projection::from_events(&store.replay(session).await.unwrap())
        );
    }
}

#[tokio::test]
async fn begin_transaction_succeeds_under_load_and_waits_behind_queued_appends() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("raw-tx-load-and-barrier");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(500))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();

    // Part A: begin_transaction does not starve under continuous concurrent appends.
    let stop = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(AtomicU64::new(0));
    let mut workers = Vec::new();
    for worker in 0..2 {
        let s = store.clone();
        let stop_flag = Arc::clone(&stop);
        let done = Arc::clone(&completed);
        let load_session = SessionId::new();
        let dir = case.dir.clone();
        workers.push(tokio::spawn(async move {
            s.append_event(load_session, &session_created_event(load_session, &dir))
                .await
                .unwrap();
            let mut n = 0_u64;
            while !stop_flag.load(Ordering::Relaxed) {
                s.append_event(
                    load_session,
                    &Event::SessionTitled {
                        session: load_session,
                        title: format!("load-{worker}-{n}"),
                    },
                )
                .await
                .unwrap();
                n += 1;
                done.fetch_add(1, Ordering::Relaxed);
            }
            n
        }));
    }
    while completed.load(Ordering::Relaxed) < 6 {
        tokio::task::yield_now().await;
    }
    for round in 0..2 {
        let mut tx = store.begin_transaction().await.unwrap();
        hya_store::append_event_in_transaction(
            &mut tx,
            session,
            Event::SessionTitled {
                session,
                title: format!("raw-tx-{round}"),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    for worker in workers {
        assert!(worker.await.unwrap() > 0);
    }
    assert_eq!(store.health(), StoreHealth::Healthy);

    // Part B: begin_transaction waits behind queued appends ahead and observes all of them.
    let barrier_session = SessionId::new();
    store
        .append_event(
            barrier_session,
            &session_created_event(barrier_session, &case.dir),
        )
        .await
        .unwrap();
    case.policy.hold_wal_writes(true);
    let s1 = store.clone();
    let app1 = tokio::spawn(async move {
        s1.append_event(
            barrier_session,
            &Event::SessionTitled {
                session: barrier_session,
                title: "queued-1".into(),
            },
        )
        .await
    });
    let pol = Arc::clone(&case.policy);
    assert!(
        tokio::task::spawn_blocking(move || pol.wait_for_held_wal_write(1, Duration::from_secs(2)))
            .await
            .unwrap()
    );
    let e2 = Event::SessionTitled {
        session: barrier_session,
        title: "queued-2".into(),
    };
    let mut app2 = Box::pin(store.append_event(barrier_session, &e2));
    assert!(poll_pending(&mut app2));
    let mut tx_fut = Box::pin(store.begin_transaction());
    assert!(poll_pending(&mut tx_fut));

    case.policy.hold_wal_writes(false);
    let mut tx = tx_fut.await.unwrap();
    let observed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_log WHERE session_id = ?")
        .bind(barrier_session.storage_key())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    app1.await.unwrap().unwrap();
    app2.await.unwrap();
    assert_eq!(observed, 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_tx_serialization_holds_appends_and_second_tx_without_degrading() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("raw-tx-serialization");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    let session = SessionId::new();
    let e1 = session_created_event(session, &case.dir);
    let (seq1, ts1) = store.append_event(session, &e1).await.unwrap();

    // 1. Append waiting behind open raw tx that rolls back waits past write_timeout without degrading.
    let mut tx_rb = store.begin_transaction().await.unwrap();
    let e_after_rb = Event::SessionTitled {
        session,
        title: "after-rollback".into(),
    };
    let mut app_rb = Box::pin(store.append_event(session, &e_after_rb));
    assert!(poll_pending(&mut app_rb));
    tokio::select! {
        res = &mut app_rb => panic!("append completed early: {res:?}"),
        () = tokio::time::sleep(Duration::from_millis(90)) => {}
    }
    assert_eq!(store.health(), StoreHealth::Healthy);
    hya_store::append_event_in_transaction(
        &mut tx_rb,
        session,
        Event::SessionTitled {
            session,
            title: "rolled-back-title".into(),
        },
    )
    .await
    .unwrap();
    tx_rb.rollback().await.unwrap();
    let (seq_after_rb, ts_after_rb) = app_rb.await.unwrap();
    assert_eq!(store.health(), StoreHealth::Healthy);

    // 2. Append waiting behind committed raw tx takes seq strictly after the tx commit.
    let mut tx_ok = store.begin_transaction().await.unwrap();
    let e_after_ok = Event::SessionTitled {
        session,
        title: "after-commit".into(),
    };
    let mut app_ok = Box::pin(store.append_event(session, &e_after_ok));
    assert!(poll_pending(&mut app_ok));
    tokio::select! {
        res = &mut app_ok => panic!("append completed early: {res:?}"),
        () = tokio::time::sleep(Duration::from_millis(90)) => {}
    }
    let env_in_tx = hya_store::append_event_in_transaction(
        &mut tx_ok,
        session,
        Event::SessionTitled {
            session,
            title: "in-raw-tx".into(),
        },
    )
    .await
    .unwrap();
    tx_ok.commit().await.unwrap();
    let (seq_after_ok, ts_after_ok) = app_ok.await.unwrap();
    assert!(seq_after_ok > env_in_tx.seq);
    assert_eq!(store.health(), StoreHealth::Healthy);

    // 3. Second raw tx behind open raw tx waits past write_timeout and succeeds without degrading.
    let tx_a = store.begin_transaction().await.unwrap();
    let mut b_fut = Box::pin(store.begin_transaction());
    assert!(poll_pending(&mut b_fut));
    tokio::select! {
        res = &mut b_fut => panic!("second raw tx returned early: {:?}", res.map(drop)),
        () = tokio::time::sleep(Duration::from_millis(90)) => {}
    }
    tx_a.commit().await.unwrap();
    b_fut.await.unwrap().commit().await.unwrap();
    assert_eq!(store.health(), StoreHealth::Healthy);

    // 4. During catch-up, begin_transaction refuses immediately if still degraded.
    case.policy.hold_wal_writes(true);
    for k in 0..2 {
        store
            .append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: format!("catchup-q{k}"),
                },
            )
            .await
            .unwrap();
    }
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
    case.policy.hold_wal_writes(false);
    if let Err(err) = store.begin_transaction().await {
        assert!(matches!(err, StoreError::StoreDegraded { .. }));
    }
    wait_until_healthy(&store, Duration::from_secs(5)).await;

    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    let disk = ro.replay(session).await.unwrap();
    assert_eq!(disk.len(), 6);
    assert_eq!(
        &disk[..4],
        &[
            Envelope {
                seq: seq1,
                ts_millis: ts1,
                event: e1,
            },
            Envelope {
                seq: seq_after_rb,
                ts_millis: ts_after_rb,
                event: e_after_rb,
            },
            env_in_tx,
            Envelope {
                seq: seq_after_ok,
                ts_millis: ts_after_ok,
                event: e_after_ok,
            },
        ]
    );
}

#[tokio::test]
async fn raw_tx_commit_updates_warm_cache_and_prevents_stale_cold_fold() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("raw-tx-cache");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();

    // Part 1: Raw tx commit updates warm cache without evicting unrelated warm sessions.
    let other = SessionId::new();
    let mut other_events = vec![session_created_event(other, &case.dir)];
    other_events.extend(message_events(other, &"w".repeat(16_384)));
    store.append_events(other, other_events).await.unwrap();
    let other_proj = store.read_projection(other).await.unwrap();

    let warm = SessionId::new();
    store
        .append_event(warm, &session_created_event(warm, &case.dir))
        .await
        .unwrap();
    let _ = store.read_projection(warm).await.unwrap();

    let mut tx = store.begin_transaction().await.unwrap();
    hya_store::append_event_in_transaction(
        &mut tx,
        warm,
        Event::SessionTitled {
            session: warm,
            title: "warm-raw-tx".into(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    case.policy.hold_wal_writes(true);
    let deg_session = SessionId::new();
    store
        .append_event(deg_session, &session_created_event(deg_session, &case.dir))
        .await
        .unwrap();
    let _ = case.policy.take_trace();
    case.policy.fail_main_reads.store(true, Ordering::Relaxed);
    let warm_proj = store.read_projection(warm).await.unwrap();
    let other_read = store.read_projection(other).await.unwrap();
    case.policy.fail_main_reads.store(false, Ordering::Relaxed);
    assert_eq!(
        case.policy
            .take_trace()
            .iter()
            .filter(|e| e.op == "xRead")
            .count(),
        0
    );
    assert_eq!(warm_proj.session.title.as_deref(), Some("warm-raw-tx"));
    assert_eq!(other_read.last_seq, other_proj.last_seq);

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;

    // Part 2: Concurrent cold fold racing a raw tx commit picks up raced commit.
    let cold = SessionId::new();
    let mut cold_initial = vec![session_created_event(cold, &case.dir)];
    cold_initial.extend(message_events(cold, &"x".repeat(16_384)));
    store.append_events(cold, cold_initial).await.unwrap();

    let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let tx_store = store.clone();
    let racer = tokio::spawn(async move {
        if trig_rx.await.is_err() {
            return;
        }
        let mut rtx = tx_store.begin_transaction().await.unwrap();
        hya_store::append_event_in_transaction(
            &mut rtx,
            cold,
            Event::SessionTitled {
                session: cold,
                title: "cold-raw-tx".into(),
            },
        )
        .await
        .unwrap();
        rtx.commit().await.unwrap();
        let _ = done_tx.send(());
    });
    case.policy.arm_shm_read_hook(2, move || {
        let _ = trig_tx.send(());
        let _ = done_rx.recv();
    });
    let r1 = store.read_projection(cold).await.unwrap();
    assert!(!case.policy.clear_shm_read_hook());
    racer.await.unwrap();
    assert_eq!(r1.session.title.as_deref(), Some("cold-raw-tx"));
}

#[tokio::test]
async fn healthy_replay_never_exposes_uncommitted_queued_op() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("healthy-replay-isolation");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_secs(1))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();

    case.policy.hold_wal_writes(true);
    let writer = store.clone();
    let app = tokio::spawn(async move {
        writer
            .append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "in-flight".into(),
                },
            )
            .await
    });
    let pol = Arc::clone(&case.policy);
    assert!(
        tokio::task::spawn_blocking(move || pol.wait_for_held_wal_write(1, Duration::from_secs(2)))
            .await
            .unwrap()
    );
    assert_eq!(store.health(), StoreHealth::Healthy);
    assert!(!app.is_finished());
    assert_eq!(store.replay(session).await.unwrap().len(), 1);
    assert_eq!(store.replay_page(session, 0, 10).await.unwrap().len(), 1);

    case.policy.hold_wal_writes(false);
    assert_eq!(app.await.unwrap().unwrap().0, EventSeq(2));
    assert_eq!(store.replay(session).await.unwrap().len(), 2);

    // Rejected event queued behind a stalled healthy op stays hidden until rejected.
    let mut tx = store.begin_transaction().await.unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_flagged_event BEFORE INSERT ON event_log \
         WHEN NEW.payload LIKE '%flagged-rejected%' \
         BEGIN SELECT RAISE(ABORT, 'rejected flagged event'); END",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    case.policy.hold_wal_writes(true);
    let w1 = store.clone();
    let app_ok = tokio::spawn(async move {
        w1.append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "ok-ahead".into(),
            },
        )
        .await
    });
    let pol2 = Arc::clone(&case.policy);
    assert!(
        tokio::task::spawn_blocking(move || pol2.wait_for_held_wal_write(2, Duration::from_secs(2)))
            .await
            .unwrap()
    );
    let e_rejected = Event::SessionTitled {
        session,
        title: "flagged-rejected".into(),
    };
    let mut app_rejected = Box::pin(store.append_event(session, &e_rejected));
    assert!(poll_pending(&mut app_rejected));
    assert_eq!(store.replay(session).await.unwrap().len(), 2);

    case.policy.hold_wal_writes(false);
    assert!(app_ok.await.unwrap().is_ok());
    assert!(app_rejected.await.is_err());
    assert_eq!(store.replay(session).await.unwrap().len(), 3);
    assert_eq!(
        store
            .read_projection(session)
            .await
            .unwrap()
            .session
            .title
            .as_deref(),
        Some("ok-ahead")
    );
}

#[tokio::test]
async fn snapshot_persist_on_read_is_bounded_when_wal_stalls() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("snapshot-persist-bound");
    let db = case.db_path("hya.db");
    let session = SessionId::new();
    {
        let prior = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        let mut batch = vec![session_created_event(session, &case.dir)];
        for i in 1..128 {
            batch.push(Event::SessionTitled {
                session,
                title: format!("snap-{i}"),
            });
        }
        prior.append_events(session, batch).await.unwrap();
    }

    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap()
            .with_projection_snapshot_interval(64);
    assert_eq!(store.health(), StoreHealth::Healthy);

    case.policy.hold_wal_writes(true);
    let proj = tokio::time::timeout(Duration::from_secs(2), store.read_projection(session))
        .await
        .expect("read_projection must bound persist_snapshot under WAL stall")
        .unwrap();
    assert_eq!(proj.last_seq, 128);
    assert!(
        case.policy
            .wait_for_held_wal_write(1, Duration::from_millis(500))
    );

    case.policy.hold_wal_writes(false);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(store.health(), StoreHealth::Healthy);
}

#[tokio::test]
async fn degraded_flush_logical_error_halts_without_dropping_or_overwriting_reason() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("degraded-flush-halt");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();

    let mut tx = store.begin_transaction().await.unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_forbidden_degraded BEFORE INSERT ON event_log \
         WHEN NEW.payload LIKE '%forbidden-degraded%' \
         BEGIN SELECT RAISE(ABORT, 'injected degraded logical fault'); END",
    )
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();

    case.policy.hold_wal_writes(true);
    let (seq2, _) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "triggers-degrade".into(),
            },
        )
        .await
        .unwrap();
    let (seq3, _) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "forbidden-degraded".into(),
            },
        )
        .await
        .unwrap();
    let (seq4, _) = store
        .append_event(
            session,
            &Event::SessionArchived {
                session,
                archived: 1.into(),
            },
        )
        .await
        .unwrap();
    assert_eq!((seq2, seq3, seq4), (EventSeq(2), EventSeq(3), EventSeq(4)));
    let initial_health = store.health();
    assert!(matches!(initial_health, StoreHealth::Degraded { .. }));

    let _ = case.policy.take_trace();
    case.policy.hold_wal_writes(false);
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while ro.replay(session).await.unwrap().len() < 2 {
        assert!(Instant::now() < deadline, "seq2 never flushed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut seen_lock = false;
    let mut seen_unlock = false;
    while !seen_unlock {
        assert!(Instant::now() < deadline, "seq3 never rolled back");
        for ev in case.policy.take_trace() {
            if ev.kind == FileKind::MainDb
                && ev.op == "xShmLock"
                && ev.offset == 0
                && ev.rc == SQLITE_OK
            {
                if ev.arg == i64::from(SQLITE_SHM_LOCK_EXCL) {
                    seen_lock = true;
                } else if seen_lock && ev.arg == i64::from(SQLITE_SHM_UNLOCK | SQLITE_SHM_EXCLUSIVE)
                {
                    seen_unlock = true;
                }
            }
        }
        if !seen_unlock {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    let _ = case.policy.take_trace();
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(count_wal_write_locks(&case.policy.take_trace()), 0);
    assert_eq!(store.health(), initial_health);
    let replayed = store.replay(session).await.unwrap();
    assert_eq!(replayed.len(), 4);
    assert_eq!(
        store.read_projection(session).await.unwrap(),
        Projection::from_events(&replayed)
    );
    assert!(store.begin_transaction().await.is_err());
    assert_eq!(ro.replay(session).await.unwrap().len(), 2);
}

#[tokio::test]
async fn queued_blobs_and_baseline_accounting_parity_and_overflow() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("queued-blobs-parity");
    let db = case.db_path("hya.db");
    let path = db.to_str().unwrap();

    // Seed an existing persisted blob to verify no double-counting after baseline reopen.
    let s_prior = SessionId::new();
    {
        let prior = SessionStore::connect(path).await.unwrap();
        prior
            .append_event(s_prior, &session_created_event(s_prior, &case.dir))
            .await
            .unwrap();
        prior.put_file_blob(s_prior, "old", b"alpha").await.unwrap();
    }

    let store = SessionStore::connect_with_write_timeout(path, Duration::from_millis(50))
        .await
        .unwrap();
    assert_eq!(store.file_blob_bytes(s_prior).await.unwrap(), 5);
    store.put_file_blob(s_prior, "old", b"alpha").await.unwrap();
    assert_eq!(store.file_blob_bytes(s_prior).await.unwrap(), 5);

    // Raw SQL blob insert after baseline is counted accurately.
    let s_raw = SessionId::new();
    store
        .append_event(s_raw, &session_created_event(s_raw, &case.dir))
        .await
        .unwrap();
    assert_eq!(store.file_blob_bytes(s_raw).await.unwrap(), 0);
    let mut tx = store.begin_transaction().await.unwrap();
    sqlx::query(
        "INSERT INTO file_blob (session_id, hash, size, content) VALUES (?, 'raw', 3, X'010203')",
    )
    .bind(s_raw.storage_key())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(store.file_blob_bytes(s_raw).await.unwrap(), 3);

    // Delete + recreate session resets budget; integer overflow is a non-storage error.
    let s_del = SessionId::new();
    store
        .append_event(s_del, &session_created_event(s_del, &case.dir))
        .await
        .unwrap();
    store.put_file_blob(s_del, "h1", b"alpha").await.unwrap();
    assert!(store.delete_session(s_del).await.unwrap());
    assert_eq!(store.file_blob_bytes(s_del).await.unwrap(), 0);
    store
        .append_event(s_del, &session_created_event(s_del, &case.dir))
        .await
        .unwrap();
    store.put_file_blob(s_del, "h2", b"charlie").await.unwrap();
    assert_eq!(store.file_blob_bytes(s_del).await.unwrap(), 7);

    let s_ov = SessionId::new();
    store
        .append_event(s_ov, &session_created_event(s_ov, &case.dir))
        .await
        .unwrap();
    let mut tx = store.begin_transaction().await.unwrap();
    for hash in ["o1", "o2"] {
        sqlx::query(
            "INSERT INTO file_blob (session_id, hash, size, content) VALUES (?, ?, ?, X'')",
        )
        .bind(s_ov.storage_key())
        .bind(hash)
        .bind(i64::MAX)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    assert!(
        !store
            .file_blob_bytes(s_ov)
            .await
            .unwrap_err()
            .is_storage_fault()
    );

    // Raw tx -> stalled blob -> append_events_with_blobs degraded parity & flush.
    let session = SessionId::new();
    let mut tx = store.begin_transaction().await.unwrap();
    let e1 = hya_store::append_event_in_transaction(
        &mut tx,
        session,
        session_created_event(session, &case.dir),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let b1 = b"alpha".to_vec();
    let b2 = b"bravo-queued".to_vec();
    let b3 = b"charlie".to_vec();
    store.put_file_blob(session, "h1", &b1).await.unwrap();

    case.policy.hold_wal_writes(true);
    store.put_file_blob(session, "h2", &b2).await.unwrap();
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
    let envs = store
        .append_events_with_blobs(
            session,
            &[("h1".into(), b1.clone()), ("h3".into(), b3.clone())],
            vec![Event::SessionTitled {
                session,
                title: "with-blobs".into(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(envs[0].seq, EventSeq(e1.seq.0 + 1));
    assert_eq!(
        store.file_blob(session, "h1").await.unwrap(),
        Some(b1.clone())
    );
    assert_eq!(
        store.file_blob(session, "h2").await.unwrap(),
        Some(b2.clone())
    );
    assert_eq!(
        store.file_blob(session, "h3").await.unwrap(),
        Some(b3.clone())
    );
    assert_eq!(
        store.file_blob_bytes(session).await.unwrap(),
        (b1.len() + b2.len() + b3.len()) as u64
    );

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
    let ro = SessionStore::connect_read_only(path).await.unwrap();
    assert_eq!(ro.file_blob(session, "h1").await.unwrap(), Some(b1));
    assert_eq!(ro.file_blob(session, "h2").await.unwrap(), Some(b2));
    assert_eq!(ro.file_blob(session, "h3").await.unwrap(), Some(b3));
    assert_eq!(ro.file_blob_bytes(session).await.unwrap(), 24);
    assert_eq!(ro.file_blob_bytes(s_prior).await.unwrap(), 5);
    assert_eq!(ro.file_blob_bytes(s_raw).await.unwrap(), 3);
}

#[tokio::test]
async fn read_timeouts_healthy_blob_waits_while_degraded_cold_read_times_out() {
    let _gate = clock_gate().read().await;

    // 1. Healthy file_blob_bytes waits past read_timeout (30ms) across a 90ms read stall.
    {
        let case = FaultCase::new("healthy-blob-read-waits");
        let db = case.db_path("hya.db");
        let cold = SessionId::new();
        {
            let prior = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
            let mut initial = vec![session_created_event(cold, &case.dir)];
            initial.extend(message_events(cold, &"r".repeat(16_384)));
            prior.append_events(cold, initial).await.unwrap();
        }
        let store = SessionStore::connect_with_timeouts(
            db.to_str().unwrap(),
            Duration::from_secs(5),
            Duration::from_millis(30),
        )
        .await
        .unwrap();
        case.policy.stall_main_reads_ms.store(90, Ordering::Relaxed);
        let started = Instant::now();
        let bytes = tokio::time::timeout(Duration::from_secs(2), store.file_blob_bytes(cold))
            .await
            .unwrap()
            .unwrap();
        let elapsed = started.elapsed();
        case.policy.stall_main_reads_ms.store(0, Ordering::Relaxed);
        assert!(
            elapsed >= Duration::from_millis(70),
            "expected healthy stall wait, got {elapsed:?}"
        );
        assert_eq!(bytes, 0);
    }

    // 2. Fresh store degraded cold read times out with StorageTimeout("read_projection").
    {
        let case = FaultCase::new("degraded-cold-read-timeout");
        let db = case.db_path("hya.db");
        let cold = SessionId::new();
        {
            let prior = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
            let mut initial = vec![session_created_event(cold, &case.dir)];
            initial.extend(message_events(cold, &"r".repeat(16_384)));
            prior.append_events(cold, initial).await.unwrap();
        }
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(60),
        )
        .await
        .unwrap();
        case.policy.hold_wal_writes(true);
        let deg = SessionId::new();
        store
            .append_event(deg, &session_created_event(deg, &case.dir))
            .await
            .unwrap();
        assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
        case.policy
            .stall_main_reads_ms
            .store(300, Ordering::Relaxed);
        let err = store.read_projection(cold).await.unwrap_err();
        case.policy.stall_main_reads_ms.store(0, Ordering::Relaxed);
        assert!(matches!(
            err,
            StoreError::StorageTimeout {
                operation: "read_projection",
                ..
            }
        ));
        case.policy.hold_wal_writes(false);
        wait_until_healthy(&store, Duration::from_secs(5)).await;
    }
}

#[tokio::test]
async fn preference_snapshot_updates_after_owner_claim_and_cancelled_commit() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("preference-snapshot-sync");
    let db = case.db_path("hya.db");
    let path = db.to_str().unwrap();

    // 1. Unlocked store refreshes preference snapshot when claiming runtime ownership.
    let unlocked = SessionStore::connect(path).await.unwrap();
    let runtime = SessionStore::connect_runtime(path).await.unwrap();
    let owner = hya_store::OwnerRunId::new();
    runtime.claim_runtime_owner(owner).unwrap();
    runtime
        .upsert_agent_effort_preference(owner, "build", "low", 1)
        .await
        .unwrap();

    // 2. Cancelled preference commit while WAL write is held resyncs owner snapshot once landed.
    case.policy.hold_wal_writes(true);
    let task_store = runtime.clone();
    let task = tokio::spawn(async move {
        task_store
            .upsert_agent_effort_preference(owner, "build", "high", 2)
            .await
    });
    let policy = Arc::clone(&case.policy);
    let entered = tokio::task::spawn_blocking(move || {
        policy.wait_for_held_wal_write(1, Duration::from_secs(2))
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    case.policy.hold_wal_writes(false);
    assert!(entered);
    runtime
        .begin_transaction()
        .await
        .unwrap()
        .rollback()
        .await
        .unwrap();

    let viewer = SessionStore::connect_read_only(path).await.unwrap();
    let sqlite = viewer.get_agent_effort_preference("build").await.unwrap();
    assert_eq!(sqlite.as_ref().unwrap().effort, "high");
    assert_eq!(
        runtime.get_agent_effort_preference("build").await.unwrap(),
        sqlite
    );

    runtime.release_runtime_owner(owner).unwrap();
    unlocked
        .claim_runtime_owner(hya_store::OwnerRunId::new())
        .unwrap();
    assert_eq!(
        unlocked.get_agent_effort_preference("build").await.unwrap(),
        sqlite
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_tx_high_water_and_cancelled_commit_guard_and_warm_rewarm() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("raw-tx-high-water");
    let db = case.db_path("hya.db");
    let path = db.to_str().unwrap();
    let store = SessionStore::connect_with_write_timeout(path, Duration::from_millis(80))
        .await
        .unwrap();
    let session = SessionId::new();
    let (seq1, _) = store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    assert_eq!(seq1, EventSeq(1));
    assert_eq!(store.read_projection(session).await.unwrap().last_seq, 1);

    // (a) Drop without commit after insert advances high-water.
    let dropped_seq = {
        let mut tx = store.begin_transaction().await.unwrap();
        let env = hya_store::append_event_in_transaction(
            &mut tx,
            session,
            Event::SessionTitled {
                session,
                title: "dropped-after-insert".into(),
            },
        )
        .await
        .unwrap();
        drop(tx);
        env.seq
    };
    let (seq_after_drop, _) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after-drop".into(),
            },
        )
        .await
        .unwrap();
    assert!(seq_after_drop > dropped_seq);

    // (b) Commit error (ENOSPC) after insert releases guard cleanly.
    let mut tx_err = store.begin_transaction().await.unwrap();
    let enospc_seq = hya_store::append_event_in_transaction(
        &mut tx_err,
        session,
        Event::SessionTitled {
            session,
            title: "x".repeat(200_000),
        },
    )
    .await
    .map(|e| e.seq);
    case.policy.enospc.store(true, Ordering::Relaxed);
    let _ = tx_err.commit().await;
    case.policy.enospc.store(false, Ordering::Relaxed);
    let (seq_after_err, _) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after-enospc".into(),
            },
        )
        .await
        .unwrap();
    if let Ok(enospc_seq) = enospc_seq {
        assert!(seq_after_err > enospc_seq);
    }

    // (c) Cancelled `begin_transaction()` while waiting behind a held op.
    case.policy.hold_wal_writes(true);
    let ahead_store = store.clone();
    let ahead = tokio::spawn(async move {
        ahead_store
            .append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "ahead".into(),
                },
            )
            .await
    });
    let pol = Arc::clone(&case.policy);
    assert!(
        tokio::task::spawn_blocking(move || pol.wait_for_held_wal_write(1, Duration::from_secs(2)))
            .await
            .unwrap()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), store.begin_transaction())
            .await
            .is_err()
    );
    case.policy.hold_wal_writes(false);
    ahead.await.unwrap().unwrap();
    wait_until_healthy(&store, Duration::from_secs(5)).await;

    // (d) Cancelled `commit()` finishing Ok(()) while reader inspects interim state:
    //     guard releases, warm fold rewarms to include the unconfirmed commit, high-water advances.
    let _ = store.read_projection(session).await.unwrap();
    let mut tx_cancel = store.begin_transaction().await.unwrap();
    let archive = hya_store::append_event_in_transaction(
        &mut tx_cancel,
        session,
        Event::SessionArchived {
            session,
            archived: 1.into(),
        },
    )
    .await
    .unwrap();
    case.policy.hold_wal_writes(true);
    let pol_c = Arc::clone(&case.policy);
    let mut entered = tokio::task::spawn_blocking(move || {
        pol_c.wait_for_held_wal_write(2, Duration::from_secs(2))
    });
    let cancelled = tokio::select! {
        _ = tx_cancel.commit() => false,
        held = &mut entered => held.unwrap(),
    };
    assert!(cancelled);
    // Interim read while COMMIT is still held sees pre-commit state.
    let interim = tokio::time::timeout(Duration::from_secs(2), store.read_projection(session))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(interim.session.archived, None);
    case.policy.hold_wal_writes(false);

    let (seq_after_cancel, _) = tokio::time::timeout(
        Duration::from_secs(3),
        store.append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after-cancelled-ok".into(),
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(seq_after_cancel > archive.seq);
    let disk_mid = SessionStore::connect_read_only(path)
        .await
        .unwrap()
        .replay(session)
        .await
        .unwrap();
    assert!(disk_mid.contains(&archive));
    assert_eq!(
        store.read_projection(session).await.unwrap(),
        Projection::from_events(&disk_mid)
    );

    // (e) Cancelled `commit()` failing with ENOSPC still releases guard for subsequent append.
    {
        let mut tx = store.begin_transaction().await.unwrap();
        let archive2 = hya_store::append_event_in_transaction(
            &mut tx,
            session,
            Event::SessionArchived {
                session,
                archived: 2.into(),
            },
        )
        .await
        .unwrap();
        case.policy.hold_wal_writes(true);
        let pol_e = Arc::clone(&case.policy);
        let mut entered = tokio::task::spawn_blocking(move || {
            pol_e.wait_for_held_wal_write(3, Duration::from_secs(2))
        });
        let cancelled = tokio::select! {
            _ = tx.commit() => false,
            held = &mut entered => held.unwrap(),
        };
        assert!(cancelled);
        case.policy.enospc.store(true, Ordering::Relaxed);
        case.policy.hold_wal_writes(false);
        let (tail_seq, _) = tokio::time::timeout(
            Duration::from_secs(2),
            store.append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "after-cancelled-err".into(),
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(tail_seq > archive2.seq);
        case.policy.enospc.store(false, Ordering::Relaxed);
        wait_until_healthy(&store, Duration::from_secs(5)).await;
    }

    // (f) Panic in caller while raw tx holds inserts releases guard cleanly.
    let p_store = store.clone();
    assert!(
        tokio::spawn(async move {
            let mut tx = p_store.begin_transaction().await.unwrap();
            hya_store::append_event_in_transaction(
                &mut tx,
                session,
                Event::SessionTitled {
                    session,
                    title: "panic-open".into(),
                },
            )
            .await
            .unwrap();
            panic!("caller panics with open raw tx");
        })
        .await
        .is_err()
    );
    store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after-panic".into(),
            },
        )
        .await
        .unwrap();

    assert_eq!(store.health(), StoreHealth::Healthy);
    let disk = SessionStore::connect_read_only(path)
        .await
        .unwrap()
        .replay(session)
        .await
        .unwrap();
    let cached = store.read_projection(session).await.unwrap();
    assert_eq!(cached, Projection::from_events(&disk));
    assert_eq!(cached.session.archived, Some(1.into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn followup_fold_preserves_first_prefix_on_read_fault_stall_and_sqlite_perm() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("followup-fold-prefix-and-perm");
    let db = case.db_path("hya.db");
    let warmup = SessionId::new();
    let cold1 = SessionId::new();
    let cold2 = SessionId::new();
    let cold3 = SessionId::new();
    {
        let prior = SessionStore::connect_runtime(db.to_str().unwrap())
            .await
            .unwrap();
        let owner = hya_store::OwnerRunId::new();
        prior.claim_runtime_owner(owner).unwrap();
        prior
            .upsert_agent_effort_preference(owner, "build", "high", 1)
            .await
            .unwrap();
        for (s, ch) in [(warmup, "w"), (cold1, "a"), (cold2, "b"), (cold3, "c")] {
            let mut events = vec![session_created_event(s, &case.dir)];
            events.extend(message_events(s, &ch.repeat(16_384)));
            prior.append_events(s, events).await.unwrap();
        }
    }

    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(40))
            .await
            .unwrap();
    let _ = store.read_projection(warmup).await.unwrap();
    assert_eq!(store.file_blob_bytes(warmup).await.unwrap(), 0);
    assert!(
        store
            .get_agent_effort_preference("build")
            .await
            .unwrap()
            .is_some()
    );

    // 1. SQLITE_PERM is classified as a storage fault across replay/blob/preference reads without degrading writes.
    case.policy
        .fail_reads_with_perm
        .store(true, Ordering::Relaxed);
    assert!(store.replay(cold3).await.unwrap_err().is_storage_fault());
    assert!(
        store
            .file_blob_bytes(cold3)
            .await
            .unwrap_err()
            .is_storage_fault()
    );
    assert!(
        store
            .get_agent_effort_preference("build")
            .await
            .unwrap_err()
            .is_storage_fault()
    );
    assert_eq!(store.health(), StoreHealth::Healthy);
    case.policy
        .fail_reads_with_perm
        .store(false, Ordering::Relaxed);

    // 2. Follow-up fold read faults (shm error, read stall, and SQLITE_PERM) return first valid prefix uncached.
    enum FaultMode {
        ShmError,
        Stall,
        Perm,
    }
    for (target, title, mode) in [
        (cold1, "during-first-fold-fault", FaultMode::ShmError),
        (cold2, "during-first-fold-stall", FaultMode::Stall),
        (cold3, "during-first-fold-perm", FaultMode::Perm),
    ] {
        let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let st = store.clone();
        let pol = Arc::clone(&case.policy);
        let is_shm = matches!(mode, FaultMode::ShmError);
        let is_stall = matches!(mode, FaultMode::Stall);
        let is_perm = matches!(mode, FaultMode::Perm);
        let racer = tokio::spawn(async move {
            if trig_rx.await.is_err() {
                return;
            }
            st.append_event(
                target,
                &Event::SessionTitled {
                    session: target,
                    title: title.into(),
                },
            )
            .await
            .unwrap();
            if is_shm {
                pol.fail_reader_shm_locks.store(true, Ordering::Relaxed);
            } else if is_stall {
                pol.stall_reader_shm_lock_ms.store(220, Ordering::Relaxed);
            } else if is_perm {
                pol.fail_reads_with_perm.store(true, Ordering::Relaxed);
            }
            let _ = done_tx.send(());
        });
        case.policy.arm_shm_read_after_locks(2, 1, move || {
            let _ = trig_tx.send(());
            let _ = done_rx.recv();
        });
        let started = Instant::now();
        let prefix = store
            .read_projection(target)
            .await
            .expect("follow-up fault must return first valid prefix");
        let elapsed = started.elapsed();
        assert!(!case.policy.clear_shm_read_hook());
        racer.await.unwrap();
        assert_eq!(prefix.session.title, None);
        assert!(prefix.last_seq > 0);
        if is_stall {
            assert!(
                elapsed < Duration::from_millis(200),
                "follow-up stall exceeded read_timeout bound: {elapsed:?}"
            );
        }
        if is_shm {
            // Confirm fallback prefix was NOT cached.
            case.policy.hold_wal_writes(true);
            store
                .append_event(
                    warmup,
                    &Event::SessionTitled {
                        session: warmup,
                        title: "degrade".into(),
                    },
                )
                .await
                .unwrap();
            assert!(store.read_projection(target).await.is_err());
            case.policy
                .fail_reader_shm_locks
                .store(false, Ordering::Relaxed);
            case.policy.hold_wal_writes(false);
            wait_until_healthy(&store, Duration::from_secs(5)).await;
        }
        case.policy
            .stall_reader_shm_lock_ms
            .store(0, Ordering::Relaxed);
        case.policy
            .fail_reads_with_perm
            .store(false, Ordering::Relaxed);
        assert_eq!(
            store
                .read_projection(target)
                .await
                .unwrap()
                .session
                .title
                .as_deref(),
            Some(title)
        );
    }
}

#[tokio::test]
async fn file_deleted_replaced_and_truncated_halt_writer_and_preserve_memory_queue() {
    let _gate = clock_gate().read().await;
    for mode in ["deleted", "replaced", "truncated"] {
        let case = FaultCase::new(&format!("file-{mode}-halt"));
        let db = case.db_path("hya.db");
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        let session = SessionId::new();
        let (s1, _) = store
            .append_event(session, &session_created_event(session, &case.dir))
            .await
            .unwrap();
        assert_eq!(s1.0, 1);
        store.read_projection(session).await.unwrap();

        match mode {
            "deleted" => std::fs::remove_file(&db).unwrap(),
            "replaced" => {
                std::fs::rename(&db, case.db_path("hya.db.moved")).unwrap();
                std::fs::write(&db, b"SENTINEL_REPLACEMENT").unwrap();
            }
            "truncated" => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&db)
                    .unwrap()
                    .set_len(0)
                    .unwrap();
            }
            _ => unreachable!(),
        }

        let (s2, _) = store
            .append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: format!("after-{mode}"),
                },
            )
            .await
            .unwrap();
        assert_eq!(s2.0, 2);
        let expected_reason = match mode {
            "deleted" => matches!(
                store.health(),
                StoreHealth::Degraded { reason } if reason.contains("missing") || reason.contains("inaccessible")
            ),
            "replaced" => matches!(
                store.health(),
                StoreHealth::Degraded { reason } if reason.contains("replaced")
            ),
            "truncated" => matches!(
                store.health(),
                StoreHealth::Degraded { reason } if reason.contains("truncated")
            ),
            _ => unreachable!(),
        };
        assert!(
            expected_reason,
            "{mode}: unexpected health {:?}",
            store.health()
        );
        assert_eq!(
            store
                .read_projection(session)
                .await
                .unwrap()
                .session
                .title
                .as_deref(),
            Some(format!("after-{mode}").as_str())
        );
        match mode {
            "deleted" => assert!(!db.exists()),
            "replaced" => assert_eq!(std::fs::read(&db).unwrap(), b"SENTINEL_REPLACEMENT"),
            "truncated" => assert_eq!(std::fs::metadata(&db).unwrap().len(), 0),
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn pk_conflict_same_event_commits_while_foreign_conflicts_halt_writer() {
    let _gate = clock_gate().read().await;

    // 1. Exact duplicate at seq 2 is treated as already committed and store recovers.
    {
        let case = FaultCase::new("pk-conflict-same");
        let db = case.db_path("hya.db");
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(40),
        )
        .await
        .unwrap();
        let session = SessionId::new();
        store
            .append_event(session, &session_created_event(session, &case.dir))
            .await
            .unwrap();
        let mut external = sqlx::SqliteConnection::connect(&format!("sqlite://{}", db.display()))
            .await
            .unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut external)
            .await
            .unwrap();
        let ev2 = Event::SessionTitled {
            session,
            title: "same-event".into(),
        };
        let envelopes = store
            .append_events(session, vec![ev2.clone()])
            .await
            .unwrap();
        assert_eq!(envelopes[0].seq.0, 2);
        assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
        sqlx::query("INSERT INTO event_log (seq, session_id, payload, ts) VALUES (?, ?, ?, ?)")
            .bind(2_i64)
            .bind(session.storage_key())
            .bind(serde_json::to_string(&ev2).unwrap())
            .bind(envelopes[0].ts_millis)
            .execute(&mut external)
            .await
            .unwrap();
        sqlx::query("COMMIT").execute(&mut external).await.unwrap();
        drop(external);

        wait_until_healthy(&store, Duration::from_secs(5)).await;
        let (seq3, _) = store
            .append_event(
                session,
                &Event::SessionArchived {
                    session,
                    archived: 1.into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(seq3.0, 3);
        let disk = SessionStore::connect_read_only(db.to_str().unwrap())
            .await
            .unwrap()
            .replay(session)
            .await
            .unwrap();
        assert_eq!(disk.len(), 3);
    }

    // 2. Foreign conflicts (different payload, different session, different timestamp) halt writer without overwriting disk.
    for (label, diff_payload, diff_session, ts_offset) in [
        ("diff-payload", true, false, 0_i64),
        ("diff-session", false, true, 0_i64),
        ("diff-ts", false, false, 1_i64),
    ] {
        let case = FaultCase::new(&format!("pk-conflict-{label}"));
        let db = case.db_path("hya.db");
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(40),
        )
        .await
        .unwrap();
        let session = SessionId::new();
        let other = SessionId::new();
        store
            .append_event(session, &session_created_event(session, &case.dir))
            .await
            .unwrap();
        if diff_session {
            store
                .append_event(other, &session_created_event(other, &case.dir))
                .await
                .unwrap();
        }
        store.read_projection(session).await.unwrap();

        let mut external = sqlx::SqliteConnection::connect(&format!("sqlite://{}", db.display()))
            .await
            .unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut external)
            .await
            .unwrap();

        let ours = Event::SessionTitled {
            session,
            title: "our-title".into(),
        };
        let envelopes = store
            .append_events(session, vec![ours.clone()])
            .await
            .unwrap();
        let conflict_seq = i64::try_from(envelopes[0].seq.0).unwrap();
        assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

        let foreign_payload = if diff_payload {
            serde_json::to_string(&Event::SessionTitled {
                session,
                title: "foreign-writer-title".into(),
            })
            .unwrap()
        } else {
            serde_json::to_string(&ours).unwrap()
        };
        let disk_session = if diff_session { other } else { session };
        sqlx::query("INSERT INTO event_log (seq, session_id, payload, ts) VALUES (?, ?, ?, ?)")
            .bind(conflict_seq)
            .bind(disk_session.storage_key())
            .bind(&foreign_payload)
            .bind(envelopes[0].ts_millis + ts_offset)
            .execute(&mut external)
            .await
            .unwrap();
        sqlx::query("COMMIT").execute(&mut external).await.unwrap();
        drop(external);

        let report = store.drain_for_exit(Duration::from_secs(2)).await;
        assert!(
            matches!(
                &report,
                ExitDrainReport::Unpersisted { events: 1, reason, .. }
                    if reason.contains("foreign writer conflict")
            ),
            "{label}: unexpected report {report:?}"
        );
        if diff_payload {
            let disk = SessionStore::connect_read_only(db.to_str().unwrap())
                .await
                .unwrap()
                .replay(session)
                .await
                .unwrap();
            assert_eq!(disk.len(), 2);
            assert_eq!(
                Projection::from_events(&disk).session.title.as_deref(),
                Some("foreign-writer-title")
            );
        }
    }
}

#[tokio::test]
async fn open_fallback_handles_stall_and_unwritable_paths_without_masking_errors() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("open-fallback-contract");

    // 1. Open stall falls back to memory within open_timeout.
    let stall_db = case.db_path("stall.db");
    case.policy.stall_open_ms.store(300, Ordering::Relaxed);
    let started = Instant::now();
    let mem_stall = SessionStore::connect_runtime_or_memory_with_timeouts(
        stall_db.to_str().unwrap(),
        Duration::from_millis(60),
        Duration::from_millis(40),
        Duration::from_millis(40),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();
    case.policy.stall_open_ms.store(0, Ordering::Relaxed);
    assert!(elapsed < Duration::from_millis(250), "elapsed={elapsed:?}");
    assert!(mem_stall.paths().is_none());
    let (fb_path, fb_reason) = mem_stall.memory_fallback().unwrap();
    assert_eq!(fb_path, stall_db);
    assert!(fb_reason.contains("timeout"), "{fb_reason}");

    // 2. Missing parent, read-only / inaccessible dirs, and ENOSPC fall back to memory.
    let missing_db = case.dir.join("no-such-parent").join("hya.db");
    let mem_missing = SessionStore::connect_runtime_or_memory(missing_db.to_str().unwrap())
        .await
        .unwrap();
    assert!(mem_missing.memory_fallback().is_some());
    let s = SessionId::new();
    mem_missing
        .append_event(s, &session_created_event(s, &case.dir))
        .await
        .unwrap();
    assert_eq!(mem_missing.replay(s).await.unwrap().len(), 1);

    if unsafe { libc::geteuid() } != 0 {
        use std::os::unix::fs::PermissionsExt as _;
        for (name, mode) in [("ro-dir", 0o500), ("inaccessible-dir", 0o000)] {
            let dir = case.dir.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
            let _guard = RestoreMode(dir.clone());
            let mem = SessionStore::connect_runtime_or_memory(dir.join("hya.db").to_str().unwrap())
                .await
                .unwrap();
            assert!(mem.memory_fallback().is_some(), "{name}");
        }
    }

    let enospc_db = case.db_path("enospc.db");
    case.policy.enospc.store(true, Ordering::Relaxed);
    let mem_enospc = SessionStore::connect_runtime_or_memory(enospc_db.to_str().unwrap())
        .await
        .unwrap();
    case.policy.enospc.store(false, Ordering::Relaxed);
    assert!(mem_enospc.memory_fallback().is_some());

    // 3. Never masks RuntimeOwnerBusy, UnsupportedDatabase, symlink lock, or sqlite URI.
    let busy_db = case.db_path("busy.db");
    let owner = SessionStore::connect_runtime(busy_db.to_str().unwrap())
        .await
        .unwrap();
    owner
        .claim_runtime_owner(hya_store::OwnerRunId::new())
        .unwrap();
    assert!(matches!(
        SessionStore::connect_runtime_or_memory(busy_db.to_str().unwrap()).await,
        Err(StoreError::RuntimeOwnerBusy)
    ));
    drop(owner);

    let legacy_db = case.dir.join("legacy.db");
    {
        let opts = SqliteConnectOptions::new()
            .filename(&legacy_db)
            .create_if_missing(true);
        let mut conn = sqlx::SqliteConnection::connect_with(&opts).await.unwrap();
        sqlx::query("CREATE TABLE event_log (id INTEGER PRIMARY KEY)")
            .execute(&mut conn)
            .await
            .unwrap();
    }
    assert!(matches!(
        SessionStore::connect_runtime_or_memory(legacy_db.to_str().unwrap()).await,
        Err(StoreError::UnsupportedDatabase { .. })
    ));

    let sym_db = case.dir.join("sym.db");
    let victim = case.dir.join("victim");
    std::fs::write(&victim, "keep").unwrap();
    std::os::unix::fs::symlink(&victim, format!("{}.runtime-owner.lock", sym_db.display()))
        .unwrap();
    assert!(matches!(
        SessionStore::connect_runtime_or_memory(sym_db.to_str().unwrap()).await,
        Err(StoreError::RuntimeOwnerLock { .. })
    ));
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    assert!(matches!(
        SessionStore::connect_runtime_or_memory(&format!("sqlite:{}", busy_db.display())).await,
        Err(StoreError::RuntimeOwnerLock { .. })
    ));
}

#[tokio::test]
async fn exit_drain_reports_clean_recovered_unpersisted_halted_fallback_and_latch() {
    let _gate = clock_gate().read().await;

    // 1. Clean report emits nothing and does NOT consume the one-shot stderr latch.
    {
        let case = FaultCase::new("drain-clean-and-latch");
        let db = case.db_path("hya.db");
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(40),
        )
        .await
        .unwrap();
        let session = SessionId::new();
        store
            .append_event(session, &session_created_event(session, &case.dir))
            .await
            .unwrap();
        let mut first = Vec::new();
        let r1 = store
            .drain_and_report_exit_to(Duration::from_secs(1), &mut first)
            .await;
        assert_eq!(r1, ExitDrainReport::Clean);
        assert_eq!(r1.format_stderr_line(), None);
        assert!(first.is_empty());

        case.policy.enospc.store(true, Ordering::Relaxed);
        let ev1 = Event::SessionTitled {
            session,
            title: "unpersisted-1".into(),
        };
        let ev2 = Event::SessionArchived {
            session,
            archived: 1.into(),
        };
        let blob = b"blob-payload-12345";
        let expected_bytes = serde_json::to_string(&ev1).unwrap().len()
            + serde_json::to_string(&ev2).unwrap().len()
            + blob.len();
        store
            .append_events_with_blobs(
                session,
                &[("hash-drain-blob-1".into(), blob.to_vec())],
                vec![ev1, ev2],
            )
            .await
            .unwrap();
        let mut out = Vec::new();
        let r2 = store
            .drain_and_report_exit_to(Duration::from_millis(120), &mut out)
            .await;
        assert!(matches!(
            &r2,
            ExitDrainReport::Unpersisted { path, events: 2, bytes, reason }
                if path == &db && *bytes == expected_bytes as u64 && !reason.is_empty()
        ));
        // Second non-clean report is silenced by latch.
        let mut out2 = Vec::new();
        assert_eq!(
            store
                .drain_and_report_exit_to(Duration::from_millis(40), &mut out2)
                .await,
            r2
        );
        assert!(out2.is_empty());
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("unpersisted") && text.contains("2 event(s)"));
        assert!(text.contains(&format!("{expected_bytes} bytes")));
        case.policy.enospc.store(false, Ordering::Relaxed);
    }

    // 2. ENOSPC cleared + WAL stall released mid-drain -> Recovered.
    {
        let case = FaultCase::new("drain-recovered");
        let db = case.db_path("hya.db");
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(40),
        )
        .await
        .unwrap();
        let session = SessionId::new();
        store
            .append_event(session, &session_created_event(session, &case.dir))
            .await
            .unwrap();
        case.policy.hold_wal_writes(true);
        store
            .append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "stall-recovered".into(),
                },
            )
            .await
            .unwrap();
        let policy = Arc::clone(&case.policy);
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            policy.hold_wal_writes(false);
        });
        let mut out = Vec::new();
        let report = store
            .drain_and_report_exit_to(Duration::from_secs(2), &mut out)
            .await;
        releaser.await.unwrap();
        assert!(matches!(
            report,
            ExitDrainReport::Recovered { ref path, ref reason } if path == &db && !reason.is_empty()
        ));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("recovered") && text.contains(&db.display().to_string()));
    }

    // 3. Halted writer (file replaced) returns immediately without waiting full 30s timeout.
    {
        let case = FaultCase::new("drain-halted");
        let db = case.db_path("hya.db");
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(40),
        )
        .await
        .unwrap();
        let session = SessionId::new();
        store
            .append_event(session, &session_created_event(session, &case.dir))
            .await
            .unwrap();
        std::fs::rename(&db, case.db_path("hya.db.moved")).unwrap();
        std::fs::write(&db, b"SENTINEL_REPLACE").unwrap();
        store
            .append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "halted".into(),
                },
            )
            .await
            .unwrap();
        let started = Instant::now();
        let report = store.drain_for_exit(Duration::from_secs(30)).await;
        assert!(started.elapsed() < Duration::from_millis(300));
        assert!(matches!(
            report,
            ExitDrainReport::Unpersisted { path, events: 1, reason, .. }
                if path == db && reason.contains("replaced")
        ));
    }

    // 4. Open fallback to memory -> InMemoryFallback with exact event & byte counts.
    if unsafe { libc::geteuid() } != 0 {
        use std::os::unix::fs::PermissionsExt as _;
        let case = FaultCase::new("drain-mem-fallback");
        let ro_dir = case.dir.join("ro");
        std::fs::create_dir_all(&ro_dir).unwrap();
        std::fs::set_permissions(&ro_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let _guard = RestoreMode(ro_dir.clone());
        let db = ro_dir.join("hya.db");
        let mem = SessionStore::connect_runtime_or_memory(db.to_str().unwrap())
            .await
            .unwrap();
        let session = SessionId::new();
        let ev1 = session_created_event(session, &case.dir);
        let ev2 = Event::SessionTitled {
            session,
            title: "mem-only".into(),
        };
        let blob = b"memory-blob-payload-xyz";
        let expected_bytes = serde_json::to_string(&ev1).unwrap().len()
            + serde_json::to_string(&ev2).unwrap().len()
            + blob.len();
        mem.append_events_with_blobs(
            session,
            &[("hash-mem-1".into(), blob.to_vec())],
            vec![ev1, ev2],
        )
        .await
        .unwrap();
        let mut out = Vec::new();
        let report = mem
            .drain_and_report_exit_to(Duration::from_secs(2), &mut out)
            .await;
        assert!(matches!(
            report,
            ExitDrainReport::InMemoryFallback { path, events: Some(2), bytes: Some(b), reason }
                if path == db && b == expected_bytes as u64 && !reason.is_empty()
        ));
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("nothing was persisted") && text.contains("2 event(s)"));
    }
}

#[tokio::test]
async fn vfs_write_stall_and_enospc_full_cycle_parity() {
    let _gate = clock_gate().read().await;
    let healthy_case = FaultCase::new("parity-healthy");
    let fault_case = FaultCase::new("parity-fault");
    let healthy_db = healthy_case.db_path("hya.db");
    let fault_db = fault_case.db_path("hya.db");

    let store_a = SessionStore::connect_with_write_timeout(
        healthy_db.to_str().unwrap(),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    let store_b = SessionStore::connect_with_write_timeout(
        fault_db.to_str().unwrap(),
        Duration::from_millis(50),
    )
    .await
    .unwrap();

    let session = SessionId::new();
    let created = session_created_event(session, &healthy_case.dir);
    store_a.append_event(session, &created).await.unwrap();
    store_b.append_event(session, &created).await.unwrap();
    store_a.read_projection(session).await.unwrap();
    store_b.read_projection(session).await.unwrap();

    let msg = MessageId::new();
    let part = PartId::new();
    let start_batch = vec![
        Event::MessageStarted {
            session,
            message: msg,
            role: Role::Assistant,
            agent: Some(AgentName::new("hya")),
            model: Some(ModelRef::new("fake/model")),
        },
        Event::TextStart {
            session,
            message: msg,
            part,
        },
    ];
    store_a
        .append_events(session, start_batch.clone())
        .await
        .unwrap();
    store_b.append_events(session, start_batch).await.unwrap();

    let mut blob_hashes = Vec::new();
    for step in 0..80_u32 {
        if step == 10 {
            fault_case.policy.hold_wal_writes(true);
            let pol = Arc::clone(&fault_case.policy);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                pol.hold_wal_writes(false);
            });
        }
        if step == 25 {
            wait_until_healthy(&store_b, Duration::from_secs(2)).await;
        }
        if step == 40 {
            fault_case.policy.enospc.store(true, Ordering::Relaxed);
        }
        if step == 60 {
            fault_case.policy.enospc.store(false, Ordering::Relaxed);
        }

        let ev = Event::TextDelta {
            session,
            message: msg,
            part,
            delta: format!("[{step:03}]"),
        };
        if step.is_multiple_of(20) {
            let hash = format!("parity-blob-{step:03}");
            let content = format!("blob-bytes-for-step-{step:03}").into_bytes();
            store_a
                .append_events_with_blobs(
                    session,
                    &[(hash.clone(), content.clone())],
                    vec![ev.clone()],
                )
                .await
                .unwrap();
            store_b
                .append_events_with_blobs(session, &[(hash.clone(), content.clone())], vec![ev])
                .await
                .unwrap();
            blob_hashes.push((hash, content));
        } else {
            store_a.append_event(session, &ev).await.unwrap();
            store_b.append_event(session, &ev).await.unwrap();
        }

        if step == 15 || step == 50 {
            assert!(matches!(store_b.health(), StoreHealth::Degraded { .. }));
            let proj_a = store_a.read_projection_shared(session).await.unwrap();
            let proj_b = store_b.read_projection_shared(session).await.unwrap();
            assert_eq!(
                proj_a.session.messages[0].parts,
                proj_b.session.messages[0].parts
            );
            assert_eq!(
                store_a.file_blob_bytes(session).await.unwrap(),
                store_b.file_blob_bytes(session).await.unwrap()
            );
            for (h, expected_bytes) in &blob_hashes {
                assert_eq!(
                    store_b.file_blob(session, h).await.unwrap().as_deref(),
                    Some(expected_bytes.as_slice())
                );
            }
        }
    }

    let finish_ev = Event::MessageFinished {
        session,
        message: msg,
        role: Role::Assistant,
        finish: FinishReason::Stop,
        tokens: None,
        cause: None,
    };
    store_a.append_event(session, &finish_ev).await.unwrap();
    store_b.append_event(session, &finish_ev).await.unwrap();

    assert_eq!(
        store_a.drain_for_exit(Duration::from_secs(5)).await,
        ExitDrainReport::Clean
    );
    assert!(matches!(
        store_b.drain_for_exit(Duration::from_secs(5)).await,
        ExitDrainReport::Recovered { path, reason } if path == fault_db && !reason.is_empty()
    ));

    let ro_a = SessionStore::connect_read_only(healthy_db.to_str().unwrap())
        .await
        .unwrap();
    let ro_b = SessionStore::connect_read_only(fault_db.to_str().unwrap())
        .await
        .unwrap();
    let disk_a = ro_a.replay(session).await.unwrap();
    let disk_b = ro_b.replay(session).await.unwrap();
    assert_eq!(disk_a.len(), 84);
    assert_eq!(disk_a.len(), disk_b.len());
    for (ea, eb) in disk_a.iter().zip(disk_b.iter()) {
        assert_eq!((ea.seq, &ea.event), (eb.seq, &eb.event));
    }
    assert_eq!(
        *store_b.read_projection_shared(session).await.unwrap(),
        Projection::from_events(&disk_b)
    );
    assert_eq!(
        ro_a.file_blob_bytes(session).await.unwrap(),
        ro_b.file_blob_bytes(session).await.unwrap()
    );
    for (h, expected_bytes) in &blob_hashes {
        assert_eq!(
            ro_b.file_blob(session, h).await.unwrap().as_deref(),
            Some(expected_bytes.as_slice())
        );
    }
}

#[tokio::test]
async fn replacement_refuses_cold_reads_raw_tx_and_answers_warm_cache() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("replacement-reads-and-tx");

    // 1. Cold read/replay/list/blob queries refuse foreign rows after replacement,
    //    while queue-only blob still answers.
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
    {
        let foreign = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        foreign
            .append_event(session, &session_created_event(session, &case.dir))
            .await
            .unwrap();
        foreign
            .append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "foreign".into(),
                },
            )
            .await
            .unwrap();
    }
    store
        .append_events_with_blobs(
            session,
            &[("qblob".into(), b"queued-blob".to_vec())],
            vec![Event::SessionArchived {
                session,
                archived: 1.into(),
            }],
        )
        .await
        .unwrap();
    assert_eq!(
        store.file_blob(session, "qblob").await.unwrap().as_deref(),
        Some(b"queued-blob".as_slice())
    );
    assert!(matches!(
        store.read_projection(session).await,
        Err(StoreError::StoreDegraded { reason }) if reason.contains("replaced")
    ));
    assert!(matches!(
        store.replay(session).await,
        Err(StoreError::StoreDegraded { reason }) if reason.contains("replaced")
    ));
    assert!(matches!(
        store.list_sessions().await,
        Err(StoreError::StoreDegraded { reason }) if reason.contains("replaced")
    ));
    assert!(matches!(
        store.file_blob_bytes(session).await,
        Err(StoreError::StoreDegraded { reason }) if reason.contains("replaced")
    ));
    assert!(matches!(
        store.file_blob(session, "missing").await,
        Err(StoreError::StoreDegraded { reason }) if reason.contains("replaced")
    ));

    // 2. Warm cached projection & session_exists answer from cache after identity halt,
    //    while cold sessions refuse.
    let db2 = case.db_path("hya2.db");
    let store2 =
        SessionStore::connect_with_write_timeout(db2.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    let warm = SessionId::new();
    let cold = SessionId::new();
    store2
        .append_event(warm, &session_created_event(warm, &case.dir))
        .await
        .unwrap();
    store2
        .append_event(cold, &session_created_event(cold, &case.dir))
        .await
        .unwrap();
    assert_eq!(
        store2.read_projection(warm).await.unwrap().session.id,
        Some(warm)
    );
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db2.display()));
    }
    assert_eq!(
        store2.read_projection(warm).await.unwrap().session.id,
        Some(warm)
    );
    assert!(store2.session_exists(warm).await.unwrap());
    assert!(matches!(
        store2.read_projection(cold).await,
        Err(StoreError::StoreDegraded { .. })
    ));
    assert!(matches!(
        store2.session_exists(cold).await,
        Err(StoreError::StoreDegraded { .. })
    ));

    // 3. Raw tx after file moves refuses with StoreDegraded and exit reports Unpersisted (even with 0 queued events).
    let db3 = case.db_path("hya3.db");
    let store3 =
        SessionStore::connect_with_write_timeout(db3.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    store3
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    std::fs::rename(&db3, case.db_path("hya3.db.moved")).unwrap();
    assert!(matches!(
        store3.begin_transaction().await,
        Err(StoreError::StoreDegraded { .. })
    ));
    assert!(matches!(
        store3.drain_for_exit(Duration::from_secs(1)).await,
        ExitDrainReport::Unpersisted {
            events: 0,
            bytes: 0,
            ..
        }
    ));

    let db4 = case.db_path("hya4.db");
    let store4 =
        SessionStore::connect_with_write_timeout(db4.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    store4
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    std::fs::rename(&db4, case.db_path("hya4.db.moved")).unwrap();
    assert!(matches!(
        store4.drain_for_exit(Duration::from_secs(1)).await,
        ExitDrainReport::Unpersisted {
            events: 0,
            bytes: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn transient_stat_error_recovers_preserves_raw_tx_health_and_clean_exit() {
    use std::os::unix::fs::PermissionsExt as _;
    let _gate = clock_gate().read().await;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    // 1. EACCES during append degrades and flushes once permissions return.
    {
        let case = FaultCase::new("transient-stat-recovers");
        let dir = case.dir.join("sub");
        std::fs::create_dir_all(&dir).unwrap();
        let _restore = RestoreMode(dir.clone());
        let db = dir.join("hya.db");
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        let session = SessionId::new();
        store
            .append_event(session, &session_created_event(session, &case.dir))
            .await
            .unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        store
            .append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "during-eacces".into(),
                },
            )
            .await
            .unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
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
        assert!(matches!(
            store.drain_for_exit(Duration::from_secs(2)).await,
            ExitDrainReport::Recovered { .. }
        ));
        let ro = SessionStore::connect_read_only(db.to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(ro.replay(session).await.unwrap().len(), 3);
    }

    // 2. EACCES before raw tx fails that tx without sticking store degraded.
    {
        let case = FaultCase::new("transient-stat-raw-tx");
        let dir = case.dir.join("sub");
        std::fs::create_dir_all(&dir).unwrap();
        let _restore = RestoreMode(dir.clone());
        let db = dir.join("hya.db");
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        let session = SessionId::new();
        store
            .append_event(session, &session_created_event(session, &case.dir))
            .await
            .unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let during = store.begin_transaction().await.map(drop);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(matches!(during, Err(StoreError::StoreDegraded { .. })));
        assert_eq!(store.health(), StoreHealth::Healthy);
        for _ in 0..3 {
            store.begin_transaction().await.unwrap();
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }

    // 3. EACCES at exit with an empty queue reports Clean with no stderr output.
    {
        let case = FaultCase::new("transient-stat-exit-empty");
        let dir = case.dir.join("sub");
        std::fs::create_dir_all(&dir).unwrap();
        let _restore = RestoreMode(dir.clone());
        let db = dir.join("hya.db");
        let store = SessionStore::connect_with_write_timeout(
            db.to_str().unwrap(),
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        let session = SessionId::new();
        store
            .append_event(session, &session_created_event(session, &case.dir))
            .await
            .unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let mut buf = Vec::new();
        let report = store
            .drain_and_report_exit_to(Duration::from_secs(2), &mut buf)
            .await;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(report, ExitDrainReport::Clean);
        assert!(buf.is_empty());
        drop(store);
        let ro = SessionStore::connect_read_only(db.to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(ro.replay(session).await.unwrap().len(), 1);
    }
}
