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
    AgentName, Envelope, Event, EventSeq, FinishReason, MailEndpoint, MailKind, MessageId,
    ModelRef, PartId, Projection, Role, SessionId, SessionKind,
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

    // Keeping the read-only pool open holds the WAL alive so SQLite never
    // checkpoints or deletes the WAL unless the writer pool retires its
    // connection and tries to close while the read-only pool is idle.
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    let _ = ro.session_exists(session).await.unwrap();

    // Hold a connection across a single monotonic jump beyond sqlx's 1,800s default max_lifetime.
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

    // Yield briefly for any background `return_to_pool` close task to start if configured.
    tokio::time::sleep(Duration::from_millis(40)).await;

    let append_res = store
        .append_events(
            session,
            vec![Event::SessionTitled {
                session,
                title: "after-drop".into(),
            }],
        )
        .await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    append_res.expect("append after 1830s jump should succeed");

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
        "unexpected WAL close / retirement activity after 1830s monotonic jump: closed={closed} locked_excl={locked_excl} hit_busy={hit_busy} trace={trace:?}"
    );

    // Also verify the read-only pool keeps connections past max_lifetime.
    case.policy.take_trace();
    let _ = ro.session_exists(session).await.unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
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
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
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

    // Inject ENOSPC during WAL commit so SQLite auto-rolls back while sqlx still
    // considers the transaction active; the store degrades on storage fault and
    // `writer_pool_options().after_release` must discard the poisoned connection
    // instead of hitting InvalidSavePointStatement when flushing after recovery.
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
        .expect("append after ENOSPC clears must enqueue cleanly");
    assert_eq!(third[0].seq, EventSeq(3));

    wait_until_healthy(&store, Duration::from_secs(5)).await;
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    let replayed = ro.replay(session).await.unwrap();
    let seqs: Vec<u64> = replayed.iter().map(|e| e.seq.0).collect();
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

struct NotifyWaker(Arc<tokio::sync::Notify>);

impl std::task::Wake for NotifyWaker {
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify_one();
    }
}

#[tokio::test]
async fn cancelled_writer_acquire_does_not_close_pinned_writer() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("writer-cancel-no-close");
    let db = case.db_path("hya.db");
    let store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
    let session = SessionId::new();
    // Seed via write only: keep the lazy read_pool unopened so any close of the
    // pinned writer handle would drop the database's last open connection.
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

    // Second poll pops the idle writer handle; with `test_before_acquire(true)`, it
    // suspends inside `check_idle_conn` ping and drops+closes the writer on `drop(fut)`.
    // With `test_before_acquire(false)`, it synchronously hands out the live handle.
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
        .expect("subsequent append after cancelled acquire must succeed");

    let trace = case.policy.take_trace();
    assert_eq!(
        trace
            .iter()
            .filter(|e| e.kind == FileKind::MainDb && e.op == "xOpen")
            .count(),
        0,
        "cancelled acquire must not force opening a replacement writer: {trace:?}"
    );
    assert_eq!(
        trace
            .iter()
            .filter(|e| e.kind == FileKind::MainDb && e.op == "xClose")
            .count(),
        0,
        "cancelled acquire must not close the pinned MainDb writer: {trace:?}"
    );
    assert_eq!(
        trace
            .iter()
            .filter(|e| e.kind == FileKind::Wal && e.op == "xClose")
            .count(),
        0,
        "cancelled acquire must not close the pinned WAL handle: {trace:?}"
    );
    assert_eq!(
        trace
            .iter()
            .filter(|e| {
                e.kind == FileKind::MainDb
                    && e.op == "xLock"
                    && e.arg == i64::from(SQLITE_LOCK_EXCLUSIVE)
            })
            .count(),
        0,
        "cancelled acquire must not trigger an exclusive WAL checkpoint lock: {trace:?}"
    );
    assert_eq!(store.replay(session).await.unwrap().len(), 2);
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
        tokio::time::sleep(Duration::from_millis(20)).await;
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
    assert_eq!(actual_proj.team, healthy_proj.team);
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

#[tokio::test]
async fn write_stall_degrades_preserves_projection_parity_and_exact_disk_seqs() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("write-stall");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
    let healthy = SessionStore::connect_memory().await.unwrap();

    // Session A starts warm in cache before the write stall (requiring `apply_cache`),
    // while Session B is created entirely during the stall.
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

    case.policy.hold_wal_writes(true);
    let title_a = Event::SessionTitled {
        session: session_a,
        title: "degraded-title".into(),
    };
    all_a.extend(
        store
            .append_events(session_a, vec![title_a.clone()])
            .await
            .unwrap(),
    );
    ref_a.extend(
        healthy
            .append_events(session_a, vec![title_a])
            .await
            .unwrap(),
    );
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    let msg_a = message_events(session_a, "reply during stall");
    all_a.extend(store.append_events(session_a, msg_a.clone()).await.unwrap());
    ref_a.extend(healthy.append_events(session_a, msg_a).await.unwrap());

    let mut batch_b = vec![session_created_event(session_b, &case.dir)];
    batch_b.extend(message_events(session_b, "new session during stall"));
    let all_b = store
        .append_events(session_b, batch_b.clone())
        .await
        .unwrap();
    let ref_b = healthy.append_events(session_b, batch_b).await.unwrap();

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
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
    let session = SessionId::new();
    let e1 = session_created_event(session, &case.dir);
    let (seq1, ts1) = store.append_event(session, &e1).await.unwrap();
    assert_eq!(seq1, EventSeq(1));
    assert_eq!(store.health(), StoreHealth::Healthy);

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
            .wait_for_held_wal_write(1, Duration::from_secs(2)),
        "timed-out op's SQLite transaction must still be in-flight inside vfs_x_write"
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

    let mut initial = vec![session_created_event(session, &case.dir)];
    for i in 1..2000 {
        initial.push(Event::SessionTitled {
            session,
            title: format!("hist-{i}"),
        });
    }
    let mut ref_envelopes = healthy
        .append_events(session, initial.clone())
        .await
        .unwrap();
    let prior = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
    let mut disk_envelopes = prior.append_events(session, initial).await.unwrap();
    assert_eq!(disk_envelopes.len(), 2000);
    drop(prior);

    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
    case.policy.hold_wal_writes(true);

    let e2001 = Event::SessionTitled {
        session,
        title: "degraded-2001".into(),
    };
    let e2002 = Event::SessionTitled {
        session,
        title: "degraded-2002".into(),
    };
    let (s2001, ts2001) = store.append_event(session, &e2001).await.unwrap();
    let (s2002, ts2002) = store.append_event(session, &e2002).await.unwrap();
    assert_eq!(s2001, EventSeq(2001));
    assert_eq!(s2002, EventSeq(2002));
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    disk_envelopes.push(Envelope {
        seq: s2001,
        ts_millis: ts2001,
        event: e2001.clone(),
    });
    disk_envelopes.push(Envelope {
        seq: s2002,
        ts_millis: ts2002,
        event: e2002.clone(),
    });
    ref_envelopes.extend(
        healthy
            .append_events(session, vec![e2001, e2002])
            .await
            .unwrap(),
    );
    assert_parity_with_healthy_store(&store, &healthy, session, &disk_envelopes, &ref_envelopes)
        .await;

    let e2003 = Event::SessionTitled {
        session,
        title: "degraded-2003".into(),
    };
    let (s2003, ts2003) = store.append_event(session, &e2003).await.unwrap();
    assert_eq!(s2003, EventSeq(2003));
    disk_envelopes.push(Envelope {
        seq: s2003,
        ts_millis: ts2003,
        event: e2003.clone(),
    });
    ref_envelopes.extend(healthy.append_events(session, vec![e2003]).await.unwrap());
    assert_parity_with_healthy_store(&store, &healthy, session, &disk_envelopes, &ref_envelopes)
        .await;

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;

    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    let replayed = ro.replay(session).await.unwrap();
    assert_eq!(replayed, disk_envelopes);
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
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
    let session = SessionId::new();
    let e1 = session_created_event(session, &case.dir);
    let (seq1, ts1) = store.append_event(session, &e1).await.unwrap();
    assert_eq!(seq1, EventSeq(1));

    let ext_opts = SqliteConnectOptions::new()
        .filename(&db)
        .busy_timeout(Duration::from_millis(120));
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
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();

    // 1. Pre-queue logical validation error.
    let mail_err = store
        .append_event(
            session,
            &Event::MailSent {
                session,
                from: "worker".into(),
                to: MailEndpoint::Handle("lead".into()),
                kind: MailKind::Message,
                body: "direct handle rejected".into(),
            },
        )
        .await
        .expect_err("unauthorized mail handle must return logical error");
    assert!(!mail_err.to_string().is_empty());
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
        .expect_err("SQLite trigger abort must be returned as logical error");
    assert!(
        trig_err.to_string().contains("injected logical error"),
        "unexpected error: {trig_err}"
    );
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
async fn non_append_while_degraded_returns_error_and_preserves_queue() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("non-append-degraded");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
    let session = SessionId::new();
    let e1 = session_created_event(session, &case.dir);
    let (seq1, ts1) = store.append_event(session, &e1).await.unwrap();
    assert_eq!(seq1, EventSeq(1));

    case.policy.hold_wal_writes(true);
    let e2 = Event::SessionTitled {
        session,
        title: "queued-while-stalled".into(),
    };
    let e3 = Event::SessionTitled {
        session,
        title: "queued-final".into(),
    };
    let (seq2, ts2) = store.append_event(session, &e2).await.unwrap();
    let (seq3, ts3) = store.append_event(session, &e3).await.unwrap();
    assert_eq!((seq2, seq3), (EventSeq(2), EventSeq(3)));
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    let tx_err = tokio::time::timeout(Duration::from_millis(500), store.begin_transaction())
        .await
        .expect("non-append while degraded must return within write_timeout")
        .expect_err("begin_transaction must fail while degraded");
    assert!(
        matches!(tx_err, hya_store::StoreError::StoreDegraded { .. }),
        "expected StoreDegraded, got {tx_err:?}"
    );
    let perm_err = tokio::time::timeout(
        Duration::from_millis(500),
        store.save_permission(&SavedPermission {
            id: "perm-1".into(),
            project_id: "global".into(),
            action: "read".into(),
            resource: "/tmp".into(),
            time_created_ms: Some(1),
        }),
    )
    .await
    .expect("non-append while degraded must return within write_timeout")
    .expect_err("save_permission must fail while degraded");
    assert!(
        matches!(perm_err, hya_store::StoreError::StoreDegraded { .. }),
        "expected StoreDegraded, got {perm_err:?}"
    );
    let del_err = tokio::time::timeout(Duration::from_millis(500), store.delete_session(session))
        .await
        .expect("non-append while degraded must return within write_timeout")
        .expect_err("delete_session must fail while degraded");
    assert!(
        matches!(del_err, hya_store::StoreError::StoreDegraded { .. }),
        "expected StoreDegraded, got {del_err:?}"
    );
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

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

#[tokio::test]
async fn healthy_cold_read_never_caches_incomplete_fold() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("healthy-cold-read");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();

    // Separate leaf/overflow pages on `read_pool` conn #1 so `other` warms
    // `sqlite_schema`/`projection_snapshot` while `session` stays cold in cache.
    let other = SessionId::new();
    let mut other_events = vec![session_created_event(other, &case.dir)];
    other_events.extend(message_events(other, &"a".repeat(16_384)));
    store.append_events(other, other_events).await.unwrap();
    let _ = store.read_projection(other).await.unwrap();

    let session = SessionId::new();
    let mut initial = vec![session_created_event(session, &case.dir)];
    initial.extend(message_events(session, &"b".repeat(16_384)));
    store.append_events(session, initial).await.unwrap();

    let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let writer_store = store.clone();
    let e2 = Event::SessionTitled {
        session,
        title: "raced-e2".into(),
    };
    let e2_for_task = e2.clone();
    let racer = tokio::spawn(async move {
        if trig_rx.await.is_err() {
            return;
        }
        writer_store
            .append_event(session, &e2_for_task)
            .await
            .unwrap();
        let _ = done_tx.send(());
    });

    case.policy.arm_shm_read_hook(2, move || {
        let _ = trig_tx.send(());
        let _ = done_rx.recv();
    });
    let r1 = store.read_projection(session).await.unwrap();
    let hook_unfired = case.policy.clear_shm_read_hook();
    racer.await.unwrap();
    assert!(
        !hook_unfired,
        "expected fold read to trigger VFS xRead hook"
    );
    assert_eq!(
        r1.session.title.as_deref(),
        Some("raced-e2"),
        "anchored follow-up fold must pick up raced commit e2"
    );

    case.policy.hold_wal_writes(true);
    let e3 = Event::SessionArchived {
        session,
        archived: 1.into(),
    };
    store.append_event(session, &e3).await.unwrap();
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    let r2 = store.read_projection(session).await.unwrap();
    assert_eq!(r2.session.title.as_deref(), Some("raced-e2"));
    assert_eq!(r2.session.archived, Some(1.into()));

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    let full_before_rounds = ro.replay(session).await.unwrap();
    assert_eq!(
        ro.read_projection(session).await.unwrap(),
        Projection::from_events(&full_before_rounds)
    );

    // Multi-round check: concurrent cold reads of fresh sessions while other sessions and the
    // same session commit continuously must always equal the prefix fold up to their own `last_seq`.
    for round in 0..4 {
        let cold_s = SessionId::new();
        let mut batch = vec![session_created_event(cold_s, &case.dir)];
        batch.extend(message_events(cold_s, &"z".repeat(4_096)));
        store.append_events(cold_s, batch).await.unwrap();

        let same_w = store.clone();
        let other_w = store.clone();
        let same_task = tokio::spawn(async move {
            for k in 0..5 {
                same_w
                    .append_event(
                        cold_s,
                        &Event::SessionTitled {
                            session: cold_s,
                            title: format!("round-{round}-{k}"),
                        },
                    )
                    .await
                    .unwrap();
            }
        });
        let other_task = tokio::spawn(async move {
            let side = SessionId::new();
            other_w
                .append_event(side, &session_created_event(side, &PathBuf::from("/tmp")))
                .await
                .unwrap();
            for k in 0..5 {
                other_w
                    .append_event(
                        side,
                        &Event::SessionTitled {
                            session: side,
                            title: format!("side-{round}-{k}"),
                        },
                    )
                    .await
                    .unwrap();
            }
        });
        for _ in 0..3 {
            let proj = store.read_projection(cold_s).await.unwrap();
            let disk = store.replay(cold_s).await.unwrap();
            let prefix: Vec<_> = disk
                .into_iter()
                .filter(|env| env.seq.0 <= proj.last_seq)
                .collect();
            assert_eq!(proj, Projection::from_events(&prefix));
        }
        same_task.await.unwrap();
        other_task.await.unwrap();
        let final_disk = store.replay(cold_s).await.unwrap();
        assert_eq!(
            store.read_projection(cold_s).await.unwrap(),
            Projection::from_events(&final_disk)
        );
    }
}

#[tokio::test]
async fn cold_read_during_catchup_succeeds_with_parity() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("cold-read-catchup");
    let db = case.db_path("hya.db");
    let other = SessionId::new();
    let session = SessionId::new();
    {
        let prior = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        let mut other_events = vec![session_created_event(other, &case.dir)];
        other_events.extend(message_events(other, &"c".repeat(16_384)));
        prior.append_events(other, other_events).await.unwrap();

        let mut initial = vec![session_created_event(session, &case.dir)];
        initial.extend(message_events(session, &"d".repeat(16_384)));
        prior.append_events(session, initial).await.unwrap();
    }

    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
    // Warm `read_pool` conn #1 on `other` while keeping `session` uncached.
    let _ = store.read_projection(other).await.unwrap();

    case.policy.hold_wal_writes(true);
    let e2 = Event::SessionTitled {
        session,
        title: "catchup-e2".into(),
    };
    let e3 = Event::SessionArchived {
        session,
        archived: 2.into(),
    };
    store.append_event(session, &e2).await.unwrap();
    store.append_event(session, &e3).await.unwrap();
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
    assert!(
        case.policy
            .wait_for_held_wal_write(1, Duration::from_secs(2))
    );

    let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let pol = Arc::clone(&case.policy);
    let store_for_catchup = store.clone();
    let flusher = tokio::spawn(async move {
        if trig_rx.await.is_err() {
            return;
        }
        pol.hold_wal_writes(false);
        wait_until_healthy(&store_for_catchup, Duration::from_secs(5)).await;
        let _ = done_tx.send(());
    });

    case.policy.arm_shm_read_hook(2, move || {
        let _ = trig_tx.send(());
        let _ = done_rx.recv();
    });
    let proj = store.read_projection(session).await.unwrap();
    let hook_unfired = case.policy.clear_shm_read_hook();
    flusher.await.unwrap();
    assert!(
        !hook_unfired,
        "expected cold fold read to trigger VFS xRead hook"
    );
    assert_eq!(store.health(), StoreHealth::Healthy);
    assert_eq!(proj.session.title.as_deref(), Some("catchup-e2"));
    assert_eq!(proj.session.archived, Some(2.into()));

    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        proj,
        Projection::from_events(&ro.replay(session).await.unwrap())
    );
}

#[tokio::test]
async fn healthy_commit_advances_warm_cache_before_degrade() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("warm-commit-before-degrade");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    // Warm `session` in ProjectionCache.
    let warm = store.read_projection(session).await.unwrap();
    assert_eq!(warm.session.title, None);

    // Commit a healthy update without calling read_projection again before degrading.
    store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "healthy-warmed".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(store.health(), StoreHealth::Healthy);

    // Degrade via a write on an unrelated session so `session`'s warm cache is
    // tested directly under degraded read_projection without any queued ops on `session`.
    case.policy.hold_wal_writes(true);
    let other = SessionId::new();
    store
        .append_event(other, &session_created_event(other, &case.dir))
        .await
        .unwrap();
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    let degraded_read = store.read_projection(session).await.unwrap();
    assert_eq!(
        degraded_read.session.title.as_deref(),
        Some("healthy-warmed")
    );

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        degraded_read,
        Projection::from_events(&ro.replay(session).await.unwrap())
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

#[tokio::test]
async fn begin_transaction_succeeds_under_continuous_append_load() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("raw-tx-under-load");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(250))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(AtomicU64::new(0));
    let mut workers = Vec::new();
    for worker in 0..4 {
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

    while completed.load(Ordering::Relaxed) < 12 {
        tokio::task::yield_now().await;
    }

    for round in 0..3 {
        let mut tx = store
            .begin_transaction()
            .await
            .expect("begin_transaction must not starve under continuous append load");
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
    assert_eq!(
        store.read_projection(session).await.unwrap(),
        Projection::from_events(&store.replay(session).await.unwrap())
    );
}

#[tokio::test]
async fn long_raw_tx_holds_appends_and_preserves_seq_order() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("raw-tx-order");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
    let session = SessionId::new();
    let e1 = session_created_event(session, &case.dir);
    let (seq1, ts1) = store.append_event(session, &e1).await.unwrap();
    assert_eq!(seq1, EventSeq(1));

    // Phase 1: append waiting behind an open raw tx that rolls back waits unboundedly without degrading.
    let mut tx_rb = store.begin_transaction().await.unwrap();
    let e_after_rb = Event::SessionTitled {
        session,
        title: "after-rollback".into(),
    };
    let mut app_rb = Box::pin(store.append_event(session, &e_after_rb));
    assert!(
        poll_pending(&mut app_rb),
        "append submitted while raw tx is open must wait until tx finishes"
    );
    let wait_rb = tokio::time::sleep(Duration::from_millis(200));
    tokio::pin!(wait_rb);
    tokio::select! {
        res = &mut app_rb => panic!("append completed early while raw tx open: {res:?}"),
        () = &mut wait_rb => {}
    }
    assert_eq!(store.health(), StoreHealth::Healthy);

    let _rolled = hya_store::append_event_in_transaction(
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
    assert_eq!(
        store
            .read_projection(session)
            .await
            .unwrap()
            .session
            .title
            .as_deref(),
        Some("after-rollback")
    );

    // Phase 2: append waiting behind an open raw tx that commits takes seq strictly after the tx commit.
    let mut tx_ok = store.begin_transaction().await.unwrap();
    let e_after_ok = Event::SessionTitled {
        session,
        title: "after-commit".into(),
    };
    let mut app_ok = Box::pin(store.append_event(session, &e_after_ok));
    assert!(
        poll_pending(&mut app_ok),
        "append submitted while raw tx is open must wait until tx commits"
    );
    let wait_ok = tokio::time::sleep(Duration::from_millis(200));
    tokio::pin!(wait_ok);
    tokio::select! {
        res = &mut app_ok => panic!("append completed early while raw tx open: {res:?}"),
        () = &mut wait_ok => {}
    }
    assert_eq!(store.health(), StoreHealth::Healthy);

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
    assert!(
        seq_after_ok > env_in_tx.seq,
        "append waiting behind committed raw tx must receive seq after tx high-water ({seq_after_ok:?} vs {:?})",
        env_in_tx.seq
    );
    assert_eq!(store.health(), StoreHealth::Healthy);

    let expected = vec![
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
    ];
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.replay(session).await.unwrap(), expected);
}

#[tokio::test]
async fn raw_tx_commit_updates_warm_cache_and_prevents_stale_cold_fold() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("raw-tx-cache");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();

    // Part 1: Raw tx commit advances warm cache entry for target session without evicting unrelated warm sessions.
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
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
    let _ = case.policy.take_trace();
    case.policy.fail_main_reads.store(true, Ordering::Relaxed);
    let warm_proj = store.read_projection(warm).await.unwrap();
    let other_read = store.read_projection(other).await.unwrap();
    case.policy.fail_main_reads.store(false, Ordering::Relaxed);
    let warm_trace = case.policy.take_trace();
    assert_eq!(
        warm_trace.iter().filter(|e| e.op == "xRead").count(),
        0,
        "degraded reads of warm cache entries must not issue disk xRead calls: {warm_trace:?}"
    );
    assert_eq!(warm_proj.session.title.as_deref(), Some("warm-raw-tx"));
    assert_eq!(other_read.last_seq, other_proj.last_seq);

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;

    // Part 2: Concurrent cold fold racing a raw tx commit must not install stale fold into cache.
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
    let hook_unfired = case.policy.clear_shm_read_hook();
    racer.await.unwrap();
    assert!(
        !hook_unfired,
        "expected cold fold read to trigger VFS xRead hook"
    );
    assert_eq!(
        r1.session.title.as_deref(),
        Some("cold-raw-tx"),
        "anchored follow-up fold must pick up raced raw tx commit"
    );

    case.policy.hold_wal_writes(true);
    let deg2 = SessionId::new();
    store
        .append_event(deg2, &session_created_event(deg2, &case.dir))
        .await
        .unwrap();
    assert_eq!(
        store
            .read_projection(cold)
            .await
            .unwrap()
            .session
            .title
            .as_deref(),
        Some("cold-raw-tx")
    );

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        store.read_projection(cold).await.unwrap(),
        Projection::from_events(&ro.replay(cold).await.unwrap())
    );
}

#[tokio::test]
async fn degraded_begin_transaction_refuses_immediately_without_blocking_appends() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("degraded-raw-tx-refusal");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(250))
            .await
            .unwrap();
    let session = SessionId::new();
    case.policy.hold_wal_writes(true);
    let (seq1, _) = store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    assert_eq!(seq1, EventSeq(1));
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    let t0 = Instant::now();
    let err = store
        .begin_transaction()
        .await
        .expect_err("begin_transaction must fail while degraded");
    assert!(
        matches!(err, hya_store::StoreError::StoreDegraded { .. }),
        "expected StoreDegraded, got {err:?}"
    );
    assert!(
        t0.elapsed() < Duration::from_millis(100),
        "degraded begin_transaction must refuse immediately without waiting write_timeout (elapsed={:?}, err={err})",
        t0.elapsed()
    );

    let t1 = Instant::now();
    let (seq2, _) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after-refused-raw-tx".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(seq2, EventSeq(2));
    assert!(
        t1.elapsed() < Duration::from_millis(100),
        "degraded append after refused begin_transaction must not block"
    );

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.replay(session).await.unwrap().len(), 2);
}

#[tokio::test]
async fn healthy_replay_never_exposes_uncommitted_queued_op() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("healthy-replay-isolation");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_secs(2))
            .await
            .unwrap();
    let session = SessionId::new();
    let (seq1, _) = store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    assert_eq!(seq1, EventSeq(1));

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
            .unwrap(),
        "expected healthy queued append to stall in vfs_x_write"
    );
    assert_eq!(store.health(), StoreHealth::Healthy);
    assert!(!app.is_finished());

    assert_eq!(store.replay(session).await.unwrap().len(), 1);
    assert_eq!(store.replay_page(session, 0, 10).await.unwrap().len(), 1);

    case.policy.hold_wal_writes(false);
    let (seq2, _) = app.await.unwrap().unwrap();
    assert_eq!(seq2, EventSeq(2));
    assert_eq!(store.replay(session).await.unwrap().len(), 2);
    assert_eq!(store.replay_page(session, 0, 10).await.unwrap().len(), 2);

    // Rejected event queued behind a stalled healthy op must also stay hidden until rejected.
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
    assert!(
        poll_pending(&mut app_rejected),
        "rejected append must enqueue synchronously behind stalled head op"
    );
    assert_eq!(store.health(), StoreHealth::Healthy);
    assert_eq!(store.replay(session).await.unwrap().len(), 2);
    assert_eq!(store.replay_page(session, 0, 10).await.unwrap().len(), 2);

    case.policy.hold_wal_writes(false);
    assert!(app_ok.await.unwrap().is_ok());
    assert!(app_rejected.await.is_err());
    let final_events = store.replay(session).await.unwrap();
    assert_eq!(final_events.len(), 3);
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
        for i in 1..1_024 {
            batch.push(Event::SessionTitled {
                session,
                title: format!("snap-{i}"),
            });
        }
        prior.append_events(session, batch).await.unwrap();
    }

    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
    assert_eq!(store.health(), StoreHealth::Healthy);

    case.policy.hold_wal_writes(true);
    let proj = tokio::time::timeout(Duration::from_secs(2), store.read_projection(session))
        .await
        .expect("read_projection must bound persist_snapshot under WAL write stall")
        .unwrap();
    assert_eq!(proj.last_seq, 1_024);
    assert!(
        case.policy
            .wait_for_held_wal_write(1, Duration::from_millis(500)),
        "expected persist_snapshot to attempt WAL write before bounding out"
    );

    case.policy.hold_wal_writes(false);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(store.health(), StoreHealth::Healthy);
}

#[tokio::test]
async fn degraded_flush_logical_error_halts_without_dropping_or_overwriting_reason() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("degraded-flush-halt");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
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
        assert!(Instant::now() < deadline, "e_stall never flushed to disk");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Wait via VFS trace for seq3 (`forbidden-degraded`) to acquire its write lock and roll back.
    let mut seen_seq3_lock = false;
    let mut seen_seq3_unlock = false;
    while !seen_seq3_unlock {
        assert!(
            Instant::now() < deadline,
            "writer never attempted and rolled back seq3 after seq2 flushed (locked={seen_seq3_lock})"
        );
        for ev in case.policy.take_trace() {
            if ev.kind == FileKind::MainDb
                && ev.op == "xShmLock"
                && ev.offset == 0
                && ev.rc == SQLITE_OK
            {
                if ev.arg == i64::from(SQLITE_SHM_LOCK_EXCL) {
                    seen_seq3_lock = true;
                } else if seen_seq3_lock
                    && ev.arg == i64::from(SQLITE_SHM_UNLOCK | SQLITE_SHM_EXCLUSIVE)
                {
                    seen_seq3_unlock = true;
                }
            }
        }
        if !seen_seq3_unlock {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    // Clear trace and wait past one retry window to verify no further write attempts reach the VFS.
    let _ = case.policy.take_trace();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let post_halt_locks = count_wal_write_locks(&case.policy.take_trace());
    assert_eq!(
        post_halt_locks, 0,
        "flush must halt on logical error without retrying writes against VFS"
    );

    assert_eq!(
        store.health(),
        initial_health,
        "halting flush on degraded logical error must preserve the first degradation reason"
    );
    let replayed = store.replay(session).await.unwrap();
    assert_eq!(replayed.len(), 4);
    assert_eq!(
        store.read_projection(session).await.unwrap(),
        Projection::from_events(&replayed)
    );
    assert!(store.begin_transaction().await.is_err());

    assert_eq!(
        ro.replay(session).await.unwrap().len(),
        2,
        "flush must halt on logical error without committing seq3 or skipping ahead to seq4"
    );
}

#[tokio::test]
async fn queued_blobs_parity_for_file_blob_and_file_blob_bytes() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("queued-blobs-parity");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();

    let b1 = b"alpha".to_vec();
    let b2 = b"bravo-queued".to_vec();
    let b3 = b"charlie".to_vec();
    store.put_file_blob(session, "h1", &b1).await.unwrap();
    assert_eq!(store.file_blob_bytes(session).await.unwrap(), 5);

    case.policy.hold_wal_writes(true);
    store.put_file_blob(session, "h2", &b2).await.unwrap();
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    store
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
        (b1.len() + b2.len() + b3.len()) as u64,
        "duplicate disk hash h1 must not double-count while queued h2 and h3 must be counted"
    );

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.file_blob(session, "h1").await.unwrap(), Some(b1));
    assert_eq!(ro.file_blob(session, "h2").await.unwrap(), Some(b2));
    assert_eq!(ro.file_blob(session, "h3").await.unwrap(), Some(b3));
    assert_eq!(ro.file_blob_bytes(session).await.unwrap(), 24);
}

#[tokio::test]
async fn degraded_cold_read_timeout_returns_storage_timeout() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("degraded-cold-read-timeout");
    let db = case.db_path("hya.db");
    let session = SessionId::new();
    {
        let prior = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        let mut initial = vec![session_created_event(session, &case.dir)];
        initial.extend(message_events(session, &"r".repeat(16_384)));
        prior.append_events(session, initial).await.unwrap();
    }

    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
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
        .store(1_000, Ordering::Relaxed);
    let err = store
        .read_projection(session)
        .await
        .expect_err("stalled cold read while degraded must return StorageTimeout");
    case.policy.stall_main_reads_ms.store(0, Ordering::Relaxed);
    assert!(
        matches!(
            err,
            hya_store::StoreError::StorageTimeout {
                operation: "read_projection",
                ..
            }
        ),
        "expected StorageTimeout(read_projection), got {err:?}"
    );
    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
}

#[tokio::test]
async fn raw_tx_then_stalled_blob_then_append_succeeds_degraded() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("tx-blob-append-degraded");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(120))
            .await
            .unwrap();
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

    case.policy.hold_wal_writes(true);
    let blob = b"stalled-blob-payload".to_vec();
    store
        .put_file_blob(session, "b-stall", &blob)
        .await
        .unwrap();
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    let e2 = Event::SessionTitled {
        session,
        title: "after-stalled-blob".into(),
    };
    let (seq2, _) = store
        .append_event(session, &e2)
        .await
        .expect("append after stalled blob must succeed degraded");
    assert_eq!(seq2, EventSeq(e1.seq.0 + 1));

    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.file_blob(session, "b-stall").await.unwrap(), Some(blob));
    assert_eq!(ro.replay(session).await.unwrap().len(), 2);
}

#[tokio::test]
async fn raw_tx_waits_behind_queued_appends_ahead() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("raw-tx-barrier-order");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_secs(2))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();

    case.policy.hold_wal_writes(true);
    let s1 = store.clone();
    let app1 = tokio::spawn(async move {
        s1.append_event(
            session,
            &Event::SessionTitled {
                session,
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
        session,
        title: "queued-2".into(),
    };
    let mut app2 = Box::pin(store.append_event(session, &e2));
    assert!(
        poll_pending(&mut app2),
        "app2 must queue synchronously behind held app1 before begin_transaction"
    );

    let mut tx_fut = Box::pin(store.begin_transaction());
    assert!(
        poll_pending(&mut tx_fut),
        "begin_transaction must wait for queued appends ahead before opening"
    );

    case.policy.hold_wal_writes(false);
    let mut tx = tx_fut.await.unwrap();
    let observed_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM event_log WHERE session_id = ?")
            .bind(session.storage_key())
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    tx.commit().await.unwrap();
    app1.await.unwrap().unwrap();
    app2.await.unwrap();
    assert_eq!(
        observed_count, 3,
        "raw transaction must observe all ops queued ahead already committed"
    );
}

#[tokio::test]
async fn cold_fold_straddling_degradation_never_caches_missed_commit() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("cold-fold-straddling-degradation");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(150))
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
    let mut other_events = vec![session_created_event(other, &case.dir)];
    other_events.extend(message_events(other, &"a".repeat(16_384)));
    store.append_events(other, other_events).await.unwrap();
    let _ = store.read_projection(other).await.unwrap();

    let session = SessionId::new();
    let mut initial = vec![session_created_event(session, &case.dir)];
    initial.extend(message_events(session, &"b".repeat(16_384)));
    store.append_events(session, initial).await.unwrap();

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
            let has_archived = ro
                .replay(session)
                .await
                .unwrap()
                .into_iter()
                .any(|e| matches!(e.event, Event::SessionArchived { .. }));
            if has_archived {
                break;
            }
            assert!(Instant::now() < deadline, "c/archived never flushed");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = done_tx.send(());
    });

    case.policy.arm_shm_read_hook(2, move || {
        let _ = trig_tx.send(());
        let _ = done_rx.recv();
    });
    let r1 = store.read_projection(session).await.unwrap();
    let hook_unfired = case.policy.clear_shm_read_hook();
    racer.await.unwrap();
    assert!(
        !hook_unfired,
        "expected cold fold read to trigger VFS xRead hook"
    );
    assert_eq!(
        r1.session.archived,
        Some(9.into()),
        "N1 refusal + N2 follow-up must include racing commits"
    );

    let r2 = store.read_projection(session).await.unwrap();
    let expected = Projection::from_events(&store.replay(session).await.unwrap());
    assert_eq!(r2, expected, "cached projection misses a committed event");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uncached_cold_read_caches_on_anchored_followup() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("anchored-followup-fold");
    let db = case.db_path("hya.db");

    // Part A: deterministic race on first fold forces Uncached -> one anchored follow-up fold caches it.
    let warmup = SessionId::new();
    let cold = SessionId::new();
    {
        let prior = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        let mut w_events = vec![session_created_event(warmup, &case.dir)];
        w_events.extend(message_events(warmup, &"w".repeat(16_384)));
        prior.append_events(warmup, w_events).await.unwrap();

        let mut c_events = vec![session_created_event(cold, &case.dir)];
        c_events.extend(message_events(cold, &"c".repeat(16_384)));
        prior.append_events(cold, c_events).await.unwrap();
    }

    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(150))
            .await
            .unwrap();
    let _ = store.read_projection(warmup).await.unwrap();

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
                title: "during-first-fold".into(),
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
    let hook_unfired = case.policy.clear_shm_read_hook();
    racer.await.unwrap();
    assert!(
        !hook_unfired,
        "expected first cold fold to fire VFS xRead hook"
    );
    assert_eq!(
        r1.session.title.as_deref(),
        Some("during-first-fold"),
        "anchored follow-up fold must pick up concurrent commit and cache it"
    );

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
    let degraded = store.read_projection(cold).await;
    let trace = case.policy.take_trace();
    case.policy.fail_main_reads.store(false, Ordering::Relaxed);
    let degraded = degraded.expect("anchored follow-up must have warmed the projection cache");
    assert_eq!(degraded.session.archived, Some(3.into()));
    assert!(
        !trace.iter().any(|ev| ev.op == "xRead"),
        "warm read must not touch disk"
    );
    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;

    // Part B: steady same-session writes keep prefix parity and converge to cached after writes stop.
    let steady = SessionId::new();
    store
        .append_event(steady, &session_created_event(steady, &case.dir))
        .await
        .unwrap();
    let mut initial = Vec::new();
    for i in 0..200 {
        initial.extend(message_events(steady, &format!("h{i}")));
    }
    store.append_events(steady, initial).await.unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let w_store = store.clone();
    let w_stop = Arc::clone(&stop);
    let writer = tokio::spawn(async move {
        let mut k = 0_u64;
        while !w_stop.load(Ordering::Relaxed) {
            w_store
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

    for _ in 0..20 {
        let proj = store.read_projection(steady).await.unwrap();
        let disk = store.replay(steady).await.unwrap();
        let prefix: Vec<_> = disk
            .into_iter()
            .filter(|e| e.seq.0 <= proj.last_seq)
            .collect();
        assert_eq!(
            proj,
            Projection::from_events(&prefix),
            "read during steady writes must match exact disk prefix"
        );
    }
    stop.store(true, Ordering::Relaxed);
    let writes = writer.await.unwrap();
    assert!(writes > 0);

    let after_stop = store.read_projection(steady).await.unwrap();
    case.policy.hold_wal_writes(true);
    let deg = SessionId::new();
    store
        .append_event(deg, &session_created_event(deg, &case.dir))
        .await
        .unwrap();
    case.policy.fail_main_reads.store(true, Ordering::Relaxed);
    let _ = case.policy.take_trace();
    let cached_after_stop = store.read_projection(steady).await.unwrap();
    let trace_after_stop = case.policy.take_trace();
    case.policy.fail_main_reads.store(false, Ordering::Relaxed);
    assert_eq!(cached_after_stop, after_stop);
    assert!(
        !trace_after_stop.iter().any(|ev| ev.op == "xRead"),
        "post-write read must be served from cache"
    );
    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_tx_high_water_advances_on_commit_drop_and_cancelled_commit() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("raw-tx-high-water");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(400))
            .await
            .unwrap();
    let session = SessionId::new();
    let (seq1, _) = store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    assert_eq!(seq1, EventSeq(1));

    // (a) Drop without commit after insert: next queued append advances past consumed seq.
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
    assert!(
        seq_after_drop > dropped_seq,
        "dropped raw tx must still advance high-water ({seq_after_drop:?} vs {dropped_seq:?})"
    );

    // (b) Commit error (ENOSPC) after insert: guard drops and releases cleanly without leaking.
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
    let _commit_res = tx_err.commit().await;
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

    // (c) Cancelled `begin_transaction()` while waiting behind a queued op.
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
        tokio::time::timeout(Duration::from_millis(50), store.begin_transaction())
            .await
            .is_err()
    );
    case.policy.hold_wal_writes(false);
    ahead.await.unwrap().unwrap();

    // (d) Cancelled `commit()` future while SQLite WAL commit is in flight:
    // SQLite finishes committing `in_flight_seq`, while the aborted task drops `RawTxGuard`.
    let mut tx_cancel = store.begin_transaction().await.unwrap();
    let in_flight_env = hya_store::append_event_in_transaction(
        &mut tx_cancel,
        session,
        Event::SessionTitled {
            session,
            title: "cancelled-commit-lands".into(),
        },
    )
    .await
    .unwrap();
    case.policy.hold_wal_writes(true);
    let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
    let pol_c = Arc::clone(&case.policy);
    let waiter = tokio::task::spawn_blocking(move || {
        let ok = pol_c.wait_for_held_wal_write(1, Duration::from_secs(2));
        let _ = held_tx.send(());
        ok
    });
    let cancelled = tokio::select! {
        res = tx_cancel.commit() => Err(res),
        _ = held_rx => Ok(()),
    };
    assert!(
        cancelled.is_ok(),
        "expected held commit future to be cancelled once WAL write entered"
    );
    assert!(
        waiter.await.unwrap(),
        "cancelled commit did not enter WAL write"
    );
    case.policy.hold_wal_writes(false);

    // Wait for the pool writer connection to finish COMMIT and return cleanly, then append.
    let (seq_after_cancel, _) = tokio::time::timeout(
        Duration::from_secs(3),
        store.append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after-cancelled-commit".into(),
            },
        ),
    )
    .await
    .expect("append blocked after cancelled commit")
    .expect("append after cancelled commit must get a fresh seq without UNIQUE constraint error");
    assert!(
        seq_after_cancel > in_flight_env.seq,
        "append after cancelled raw commit must allocate strictly higher seq ({seq_after_cancel:?} vs {:?})",
        in_flight_env.seq
    );

    // (e) Panic in caller while raw transaction holds inserts.
    let p_store = store.clone();
    let panicked = tokio::spawn(async move {
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
    .await;
    assert!(panicked.is_err());
    let (seq_after_panic, _) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after-panic".into(),
            },
        )
        .await
        .unwrap();
    assert!(seq_after_panic > seq_after_cancel);

    assert_eq!(store.health(), StoreHealth::Healthy);
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    let disk = ro.replay(session).await.unwrap();
    assert_eq!(
        store.read_projection(session).await.unwrap(),
        Projection::from_events(&disk)
    );
    assert!(
        disk.iter()
            .any(|e| matches!(&e.event, Event::SessionTitled { title, .. } if title == "after-cancelled-commit"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthy_raw_tx_waits_behind_open_raw_tx_and_degraded_refuses_in_catchup() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("two-raw-tx-serialization");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(200))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();

    // Part 1: on a healthy store, a second raw tx behind an open raw tx held longer than
    // `write_timeout` waits (as on `main`) and succeeds without degrading.
    let tx_a = store.begin_transaction().await.unwrap();
    let mut b_fut = Box::pin(store.begin_transaction());
    assert!(
        poll_pending(&mut b_fut),
        "second raw tx must wait behind held tx_a"
    );
    let timer = tokio::time::sleep(Duration::from_millis(350));
    tokio::pin!(timer);
    let b_waited = tokio::select! {
        res = &mut b_fut => Err(res.map(drop)),
        () = &mut timer => Ok(()),
    };
    assert!(
        b_waited.is_ok(),
        "second raw tx returned early instead of waiting past write_timeout: {b_waited:?}"
    );
    tx_a.commit().await.unwrap();
    let tx_b = b_fut
        .await
        .expect("second raw tx on healthy store must succeed after first commits");
    tx_b.commit().await.unwrap();
    assert_eq!(store.health(), StoreHealth::Healthy);

    // Part 2: during catch-up with disk working, `begin_transaction` refuses immediately
    // rather than holding appends behind it.
    case.policy.hold_wal_writes(true);
    for k in 0..3 {
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
    // Queue is still draining or just drained; if degraded at check time, refusal is immediate.
    let err_or_ok = store.begin_transaction().await;
    if let Err(err) = err_or_ok {
        assert!(
            matches!(err, hya_store::StoreError::StoreDegraded { .. }),
            "expected immediate StoreDegraded during catch-up, got {err:?}"
        );
    }
    wait_until_healthy(&store, Duration::from_secs(5)).await;
    assert_eq!(store.replay(session).await.unwrap().len(), 4);
}

#[tokio::test]
async fn file_blob_bytes_deleted_recreated_and_overflow_error() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("blob-del-overflow");
    let db = case.db_path("hya.db");
    let store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();

    let s = SessionId::new();
    store
        .append_event(s, &session_created_event(s, &case.dir))
        .await
        .unwrap();
    store.put_file_blob(s, "h1", b"alpha").await.unwrap();
    assert_eq!(store.file_blob_bytes(s).await.unwrap(), 5);

    assert!(store.delete_session(s).await.unwrap());
    assert_eq!(store.file_blob_bytes(s).await.unwrap(), 0);

    // Reusing the session id returns SQLite's exact budget.
    store
        .append_event(s, &session_created_event(s, &case.dir))
        .await
        .unwrap();
    store.put_file_blob(s, "h2", b"charlie").await.unwrap();
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(store.file_blob_bytes(s).await.unwrap(), 7);
    assert_eq!(ro.file_blob_bytes(s).await.unwrap(), 7);

    // Invalid size overflow causes SQLite integer overflow (non-storage error, not omitted).
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
    let err = store.file_blob_bytes(s_ov).await.unwrap_err();
    assert!(
        !err.is_storage_fault(),
        "integer overflow must not be classified as a storage fault: {err:?}"
    );
}

#[tokio::test]
async fn existing_blob_hash_after_baseline_avoids_double_counting() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("existing-blob-hash");
    let db = case.db_path("hya.db");
    let path = db.to_str().unwrap();
    let session = SessionId::new();
    {
        let prior = SessionStore::connect(path).await.unwrap();
        prior
            .append_event(session, &session_created_event(session, &case.dir))
            .await
            .unwrap();
        prior.put_file_blob(session, "old", b"alpha").await.unwrap();
    }
    let store = SessionStore::connect(path).await.unwrap();
    assert_eq!(store.file_blob_bytes(session).await.unwrap(), 5);
    store.put_file_blob(session, "old", b"alpha").await.unwrap();
    let viewer = SessionStore::connect_read_only(path).await.unwrap();
    let sqlite = viewer.file_blob_bytes(session).await.unwrap();
    let cached = store.file_blob_bytes(session).await.unwrap();
    println!("re-put persisted hash after baseline: SQLite={sqlite}, cached={cached}");
    assert_eq!(sqlite, 5);
    assert_eq!(cached, sqlite);
}

#[tokio::test]
async fn raw_blob_insert_after_baseline_is_counted() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("raw-blob-insert");
    let db = case.db_path("hya.db");
    let path = db.to_str().unwrap();
    let session = SessionId::new();
    let store = SessionStore::connect(path).await.unwrap();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    assert_eq!(store.file_blob_bytes(session).await.unwrap(), 0);
    let mut tx = store.begin_transaction().await.unwrap();
    sqlx::query(
        "INSERT INTO file_blob (session_id, hash, size, content) VALUES (?, 'raw', 3, X'010203')",
    )
    .bind(session.storage_key())
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let viewer = SessionStore::connect_read_only(path).await.unwrap();
    let sqlite = viewer.file_blob_bytes(session).await.unwrap();
    let cached = store.file_blob_bytes(session).await.unwrap();
    println!("raw blob insert: SQLite={sqlite}, cached={cached}");
    assert_eq!(sqlite, 3);
    assert_eq!(cached, sqlite);
}

#[tokio::test]
async fn file_blob_bytes_waits_past_read_timeout_when_healthy() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("blob-waits-when-healthy");
    let db = case.db_path("hya.db");
    let store = SessionStore::connect_with_timeouts(
        db.to_str().unwrap(),
        Duration::from_secs(5),
        Duration::from_millis(40),
    )
    .await
    .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    // Stall reads for 150ms (> 40ms read_timeout, << 5s write_timeout) while write queue is healthy.
    case.policy
        .stall_main_reads_ms
        .store(150, Ordering::Relaxed);
    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(2), store.file_blob_bytes(session)).await;
    let elapsed = started.elapsed();
    case.policy.stall_main_reads_ms.store(0, Ordering::Relaxed);
    println!(
        "healthy blob read across 150ms stall with 40ms read_timeout: elapsed={elapsed:?}, result={result:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(120),
        "test must observe stall past 40ms read_timeout, elapsed={elapsed:?}"
    );
    assert_eq!(result.unwrap().unwrap(), 0);
}

#[tokio::test]
async fn unlocked_preference_snapshot_updates_after_owner_claim() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("unlocked-preference-snapshot");
    let db = case.db_path("hya.db");
    let path = db.to_str().unwrap();
    let unlocked = SessionStore::connect(path).await.unwrap();
    let runtime = SessionStore::connect_runtime(path).await.unwrap();
    let owner = hya_store::OwnerRunId::new();
    runtime.claim_runtime_owner(owner).unwrap();
    runtime
        .upsert_agent_effort_preference(owner, "build", "high", 4)
        .await
        .unwrap();
    runtime.release_runtime_owner(owner).unwrap();
    unlocked
        .claim_runtime_owner(hya_store::OwnerRunId::new())
        .unwrap();
    let viewer = SessionStore::connect_read_only(path).await.unwrap();
    let sqlite = viewer.get_agent_effort_preference("build").await.unwrap();
    let snapshot = unlocked.get_agent_effort_preference("build").await.unwrap();
    println!("successful new owner claim: SQLite={sqlite:?}, unlocked snapshot={snapshot:?}");
    assert_eq!(snapshot, sqlite);
}

#[tokio::test]
async fn cancelled_preference_commit_resyncs_snapshot() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("cancelled-preference-commit");
    let db = case.db_path("hya.db");
    let path = db.to_str().unwrap();
    let runtime = SessionStore::connect_runtime(path).await.unwrap();
    let owner = hya_store::OwnerRunId::new();
    runtime.claim_runtime_owner(owner).unwrap();
    runtime
        .upsert_agent_effort_preference(owner, "build", "low", 1)
        .await
        .unwrap();

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
    assert!(entered, "SQLite did not reach the commit gate");
    runtime
        .begin_transaction()
        .await
        .unwrap()
        .rollback()
        .await
        .unwrap();

    let viewer = SessionStore::connect_read_only(path).await.unwrap();
    let sqlite = viewer.get_agent_effort_preference("build").await.unwrap();
    let snapshot = runtime.get_agent_effort_preference("build").await.unwrap();
    println!("cancelled preference commit: SQLite={sqlite:?}, owner snapshot={snapshot:?}");
    assert_eq!(sqlite.as_ref().unwrap().effort, "high");
    assert_eq!(snapshot, sqlite);
}

#[tokio::test]
async fn cancelled_raw_commit_preserves_warm_fold_completeness() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("cancelled-raw-warm-fold");
    let db = case.db_path("hya.db");
    let path = db.to_str().unwrap();
    let store = SessionStore::connect(path).await.unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    assert_eq!(
        store
            .read_projection(session)
            .await
            .unwrap()
            .session
            .archived,
        None
    );
    let mut tx = store.begin_transaction().await.unwrap();
    let raw = hya_store::append_event_in_transaction(
        &mut tx,
        session,
        Event::SessionArchived {
            session,
            archived: 1.into(),
        },
    )
    .await
    .unwrap();
    case.policy.hold_wal_writes(true);
    let policy = Arc::clone(&case.policy);
    let entered = tokio::task::spawn_blocking(move || {
        policy.wait_for_held_wal_write(1, Duration::from_secs(2))
    });
    tokio::pin!(entered);
    let confirmed = tokio::select! {
        result = tx.commit() => Some(result),
        result = &mut entered => { assert!(result.unwrap()); None }
    };
    case.policy.hold_wal_writes(false);
    assert!(confirmed.is_none());

    let (tail, _) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "published suffix".into(),
            },
        )
        .await
        .unwrap();
    assert!(tail > raw.seq);
    let viewer = SessionStore::connect_read_only(path).await.unwrap();
    let disk = viewer.replay(session).await.unwrap();
    assert!(disk.contains(&raw));
    let sqlite = Projection::from_events(&disk);
    let cached = store.read_projection(session).await.unwrap();
    println!(
        "cancelled raw commit {}, next append {}: SQLite archived={:?}, cached archived={:?}",
        raw.seq.0, tail.0, sqlite.session.archived, cached.session.archived
    );
    assert_eq!(cached.session.archived, sqlite.session.archived);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn followup_fold_preserves_first_prefix_on_read_fault_and_stall() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("followup-fold-prefix");
    let db = case.db_path("hya.db");
    let warmup = SessionId::new();
    let cold1 = SessionId::new();
    let cold2 = SessionId::new();
    {
        let prior = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
        for (session, ch) in [(warmup, "w"), (cold1, "a"), (cold2, "b")] {
            let mut events = vec![session_created_event(session, &case.dir)];
            events.extend(message_events(session, &ch.repeat(16_384)));
            prior.append_events(session, events).await.unwrap();
        }
    }

    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
            .await
            .unwrap();
    let _ = store.read_projection(warmup).await.unwrap();

    // Part A: follow-up read fault returns the first valid prefix uncached.
    // Fire after Query 2 (`fold`) has locked its WAL snapshot (`walTryBeginRead` complete).
    let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let st = store.clone();
    let policy_a = Arc::clone(&case.policy);
    let racer_a = tokio::spawn(async move {
        if trig_rx.await.is_err() {
            return;
        }
        st.append_event(
            cold1,
            &Event::SessionTitled {
                session: cold1,
                title: "during-first-fold-fault".into(),
            },
        )
        .await
        .unwrap();
        policy_a
            .fail_reader_shm_locks
            .store(true, Ordering::Relaxed);
        let _ = done_tx.send(());
    });
    case.policy.arm_shm_read_after_locks(2, 1, move || {
        let _ = trig_tx.send(());
        let _ = done_rx.recv();
    });
    let r1 = store
        .read_projection(cold1)
        .await
        .expect("follow-up read fault must return first valid prefix instead of error");
    assert!(!case.policy.clear_shm_read_hook());
    racer_a.await.unwrap();
    assert_eq!(r1.session.title, None);
    assert!(r1.last_seq > 0);

    // Prove r1 was NOT cached: while degraded and reader shm locks fail, read_projection misses cache and fails.
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
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
    assert!(
        store.read_projection(cold1).await.is_err(),
        "fallback prefix must remain uncached"
    );
    case.policy
        .fail_reader_shm_locks
        .store(false, Ordering::Relaxed);
    case.policy.hold_wal_writes(false);
    wait_until_healthy(&store, Duration::from_secs(5)).await;
    assert_eq!(
        store
            .read_projection(cold1)
            .await
            .unwrap()
            .session
            .title
            .as_deref(),
        Some("during-first-fold-fault")
    );

    // Part B: follow-up read stall while healthy writes is cut at READ_TIMEOUT and returns first prefix.
    let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let st = store.clone();
    let policy_b = Arc::clone(&case.policy);
    let racer_b = tokio::spawn(async move {
        if trig_rx.await.is_err() {
            return;
        }
        st.append_event(
            cold2,
            &Event::SessionTitled {
                session: cold2,
                title: "during-first-fold-stall".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(st.health(), StoreHealth::Healthy));
        policy_b
            .stall_reader_shm_lock_ms
            .store(500, Ordering::Relaxed);
        let _ = done_tx.send(());
    });
    case.policy.arm_shm_read_after_locks(2, 1, move || {
        let _ = trig_tx.send(());
        let _ = done_rx.recv();
    });
    let started = Instant::now();
    let r2 = store
        .read_projection(cold2)
        .await
        .expect("stalled follow-up must return first valid prefix at read_timeout");
    let elapsed = started.elapsed();
    assert!(!case.policy.clear_shm_read_hook());
    case.policy
        .stall_reader_shm_lock_ms
        .store(0, Ordering::Relaxed);
    racer_b.await.unwrap();
    assert!(
        elapsed < Duration::from_millis(350),
        "follow-up stall exceeded read_timeout bound: {elapsed:?}"
    );
    assert!(matches!(store.health(), StoreHealth::Healthy));
    assert_eq!(r2.session.title, None);
    assert_eq!(
        store
            .read_projection(cold2)
            .await
            .unwrap()
            .session
            .title
            .as_deref(),
        Some("during-first-fold-stall")
    );
}

#[tokio::test]
async fn cancelled_commit_reader_rewarms_when_commit_finishes_late() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("cancelled-commit-rewarm");
    let db = case.db_path("hya.db");
    let path = db.to_str().unwrap();
    let store = SessionStore::connect(path).await.unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    assert_eq!(store.read_projection(session).await.unwrap().last_seq, 1);
    let mut tx = store.begin_transaction().await.unwrap();
    let archive = hya_store::append_event_in_transaction(
        &mut tx,
        session,
        Event::SessionArchived {
            session,
            archived: 1.into(),
        },
    )
    .await
    .unwrap();
    case.policy.hold_wal_writes(true);
    let policy = Arc::clone(&case.policy);
    let mut entered = tokio::task::spawn_blocking(move || {
        policy.wait_for_held_wal_write(1, Duration::from_secs(2))
    });
    let cancelled_at_gate = tokio::select! {
        result = tx.commit() => {
            eprintln!("COMMIT returned before the gate: {result:?}");
            false
        }
        held = &mut entered => held.unwrap(),
    };

    // The cancelled Rust future has dropped its guard. SQLite is still in COMMIT.
    let interim =
        tokio::time::timeout(Duration::from_secs(2), store.read_projection(session)).await;
    case.policy.hold_wal_writes(false);
    assert!(cancelled_at_gate, "SQLite did not enter the COMMIT gate");
    let interim = interim.expect("read while COMMIT held").unwrap();
    assert_eq!(interim.last_seq, 1);
    assert_eq!(interim.session.archived, None);

    let (title, _) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after commit".into(),
            },
        )
        .await
        .unwrap();
    let disk = SessionStore::connect_read_only(path)
        .await
        .unwrap()
        .replay(session)
        .await
        .unwrap();
    assert!(
        disk.contains(&archive),
        "the unconfirmed archive must have committed"
    );
    assert!(title > archive.seq);
    assert_eq!(store.health(), StoreHealth::Healthy);
    let disk = Projection::from_events(&disk);
    let cached = store.read_projection(session).await.unwrap();
    println!(
        "interim seq {}, archive seq {}, later title seq {}: disk archive {:?}, cached archive {:?}",
        interim.last_seq, archive.seq.0, title.0, disk.session.archived, cached.session.archived
    );
    assert_eq!(cached.last_seq, disk.last_seq);
    assert_eq!(cached.session.archived, disk.session.archived);
}

#[tokio::test]
async fn cancelled_commit_releases_guard_for_next_append() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("cancelled-commit-guard-release");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();

    // Case A: cancelled commit finishes Ok(()) -> guard releases so next append proceeds and state matches disk.
    {
        let mut tx = store.begin_transaction().await.unwrap();
        let archive = hya_store::append_event_in_transaction(
            &mut tx,
            session,
            Event::SessionArchived {
                session,
                archived: 1.into(),
            },
        )
        .await
        .unwrap();
        case.policy.hold_wal_writes(true);
        let policy = Arc::clone(&case.policy);
        let mut entered = tokio::task::spawn_blocking(move || {
            policy.wait_for_held_wal_write(1, Duration::from_secs(2))
        });
        let cancelled = tokio::select! {
            _ = tx.commit() => false,
            held = &mut entered => held.unwrap(),
        };
        assert!(cancelled);
        case.policy.hold_wal_writes(false);

        let (title_seq, _) = tokio::time::timeout(
            Duration::from_secs(2),
            store.append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "after cancelled ok commit".into(),
                },
            ),
        )
        .await
        .expect("guard must release after spawned commit completes")
        .unwrap();
        assert!(title_seq > archive.seq);
    }

    // Case B: cancelled commit fails with ENOSPC -> guard releases on Err path so next append proceeds.
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
        let policy = Arc::clone(&case.policy);
        let mut entered = tokio::task::spawn_blocking(move || {
            policy.wait_for_held_wal_write(2, Duration::from_secs(2))
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
                    title: "after cancelled err commit".into(),
                },
            ),
        )
        .await
        .expect("guard must release after failed spawned commit")
        .unwrap();
        assert!(tail_seq > archive2.seq);
        case.policy.enospc.store(false, Ordering::Relaxed);
        wait_until_healthy(&store, Duration::from_secs(5)).await;

        let disk = SessionStore::connect_read_only(db.to_str().unwrap())
            .await
            .unwrap()
            .replay(session)
            .await
            .unwrap();
        let disk_proj = Projection::from_events(&disk);
        let cached = store.read_projection(session).await.unwrap();
        assert_eq!(cached, disk_proj);
        assert_eq!(cached.session.archived, Some(1.into()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_perm_classified_as_storage_fault_across_fallback_reads() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("sqlite-perm-fallback");
    let db = case.db_path("hya.db");
    let session = SessionId::new();
    let warmup = SessionId::new();
    let cold = SessionId::new();
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
        for (s, ch) in [(session, "s"), (warmup, "w"), (cold, "c")] {
            let mut events = vec![session_created_event(s, &case.dir)];
            events.extend(message_events(s, &ch.repeat(16_384)));
            prior.append_events(s, events).await.unwrap();
        }
    }

    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
            .await
            .unwrap();
    // Warm `read_pool` on healthy disk first so read queries exercise xShmLock directly.
    let _ = store.read_projection(warmup).await.unwrap();
    assert_eq!(store.file_blob_bytes(warmup).await.unwrap(), 0);
    assert!(
        store
            .get_agent_effort_preference("build")
            .await
            .unwrap()
            .is_some()
    );

    // Inject SQLITE_PERM (3) on reader shm locks: all three read-fallback error checks see
    // `is_storage_fault() == true`, and slow/failing reads never degrade the write queue.
    case.policy
        .fail_reads_with_perm
        .store(true, Ordering::Relaxed);
    let replay_err = store.replay(session).await.unwrap_err();
    assert!(replay_err.is_storage_fault(), "replay: {replay_err}");
    let blob_err = store.file_blob_bytes(session).await.unwrap_err();
    assert!(blob_err.is_storage_fault(), "file_blob_bytes: {blob_err}");
    let pref_err = store
        .get_agent_effort_preference("build")
        .await
        .unwrap_err();
    assert!(pref_err.is_storage_fault(), "preference read: {pref_err}");
    assert_eq!(store.health(), StoreHealth::Healthy);
    case.policy
        .fail_reads_with_perm
        .store(false, Ordering::Relaxed);

    // Also exercise `cached_projection` anchored follow-up fold under SQLITE_PERM:
    // the fallback returns the first valid prefix instead of propagating SQLITE_PERM.
    let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let st = store.clone();
    let policy_racer = Arc::clone(&case.policy);
    let racer = tokio::spawn(async move {
        if trig_rx.await.is_err() {
            return;
        }
        st.append_event(
            cold,
            &Event::SessionTitled {
                session: cold,
                title: "during-first-fold-perm".into(),
            },
        )
        .await
        .unwrap();
        policy_racer
            .fail_reads_with_perm
            .store(true, Ordering::Relaxed);
        let _ = done_tx.send(());
    });
    case.policy.arm_shm_read_after_locks(2, 1, move || {
        let _ = trig_tx.send(());
        let _ = done_rx.recv();
    });
    let prefix = store
        .read_projection(cold)
        .await
        .expect("follow-up SQLITE_PERM fault must fall back to first valid prefix");
    assert!(!case.policy.clear_shm_read_hook());
    racer.await.unwrap();
    assert_eq!(prefix.session.title, None);
    assert!(prefix.last_seq > 0);
    case.policy
        .fail_reads_with_perm
        .store(false, Ordering::Relaxed);
    assert_eq!(
        store
            .read_projection(cold)
            .await
            .unwrap()
            .session
            .title
            .as_deref(),
        Some("during-first-fold-perm")
    );
}

#[tokio::test]
async fn file_deleted_halts_writer_keeps_queue_in_memory() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("file-deleted-halt");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
            .await
            .unwrap();
    let session = SessionId::new();
    let (s1, _) = store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    assert_eq!(s1.0, 1);
    store.read_projection(session).await.unwrap();

    std::fs::remove_file(&db).unwrap();

    let (s2, _) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after-delete".into(),
            },
        )
        .await
        .unwrap();
    let (s3, _) = store
        .append_event(
            session,
            &Event::SessionArchived {
                session,
                archived: 1.into(),
            },
        )
        .await
        .unwrap();
    assert_eq!((s2.0, s3.0), (2, 3));
    assert!(matches!(
        store.health(),
        StoreHealth::Degraded { reason } if reason.contains("missing") || reason.contains("inaccessible")
    ));
    let proj = store.read_projection(session).await.unwrap();
    assert_eq!(proj.session.title.as_deref(), Some("after-delete"));
    assert_eq!(proj.session.archived, Some(1.into()));
    assert!(!db.exists(), "halted writer must not recreate deleted db");
}

#[tokio::test]
async fn file_replaced_halts_writer_never_writes_to_replacement() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("file-replaced-halt");
    let db = case.db_path("hya.db");
    let moved = case.db_path("hya.db.moved");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    store.read_projection(session).await.unwrap();

    std::fs::rename(&db, &moved).unwrap();
    std::fs::write(&db, b"SENTINEL_REPLACEMENT").unwrap();

    let (s2, _) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after-replace".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(s2.0, 2);
    assert!(matches!(
        store.health(),
        StoreHealth::Degraded { reason } if reason.contains("replaced")
    ));
    let proj = store.read_projection(session).await.unwrap();
    assert_eq!(proj.session.title.as_deref(), Some("after-replace"));
    assert_eq!(std::fs::read(&db).unwrap(), b"SENTINEL_REPLACEMENT");
}

#[tokio::test]
async fn file_truncated_degrades_and_keeps_queue_in_memory() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("file-truncated-halt");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    store.read_projection(session).await.unwrap();

    std::fs::OpenOptions::new()
        .write(true)
        .open(&db)
        .unwrap()
        .set_len(0)
        .unwrap();

    let (s2, _) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after-truncate".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(s2.0, 2);
    assert!(matches!(
        store.health(),
        StoreHealth::Degraded { reason } if reason.contains("truncated")
    ));
    let proj = store.read_projection(session).await.unwrap();
    assert_eq!(proj.session.title.as_deref(), Some("after-truncate"));
    assert_eq!(std::fs::metadata(&db).unwrap().len(), 0);
}

#[tokio::test]
async fn pk_conflict_same_event_treated_as_committed() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("pk-conflict-same");
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
    store.read_projection(session).await.unwrap();

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
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].seq.0, 2);
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    // Simulate an earlier commit of this exact event landing before the retry executes.
    let payload2 = serde_json::to_string(&ev2).unwrap();
    sqlx::query("INSERT INTO event_log (seq, session_id, payload, ts) VALUES (?, ?, ?, ?)")
        .bind(2_i64)
        .bind(session.storage_key())
        .bind(&payload2)
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
    assert_eq!(disk[1].seq.0, 2);
    assert_eq!(disk[2].seq.0, 3);
}

#[tokio::test]
async fn pk_conflict_different_event_halts_writer() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("pk-conflict-diff");
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
    let envelopes = store.append_events(session, vec![ours]).await.unwrap();
    assert_eq!(envelopes[0].seq.0, 2);
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    // A foreign writer commits a DIFFERENT event at seq 2.
    let foreign = Event::SessionTitled {
        session,
        title: "foreign-writer-title".into(),
    };
    let foreign_payload = serde_json::to_string(&foreign).unwrap();
    sqlx::query("INSERT INTO event_log (seq, session_id, payload, ts) VALUES (?, ?, ?, ?)")
        .bind(2_i64)
        .bind(session.storage_key())
        .bind(&foreign_payload)
        .bind(envelopes[0].ts_millis)
        .execute(&mut external)
        .await
        .unwrap();
    sqlx::query("COMMIT").execute(&mut external).await.unwrap();
    drop(external);

    // Give the writer task a brief moment to process the released lock and halt on conflict.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Err(hya_store::StoreError::StoreDegraded { reason }) =
            store.begin_transaction().await
            && reason.contains("foreign writer conflict")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "writer did not record foreign writer conflict: {:?}",
            store.health()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

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
    let mem_proj = store.read_projection(session).await.unwrap();
    assert_eq!(mem_proj.session.title.as_deref(), Some("our-title"));
    assert_eq!(mem_proj.session.archived, Some(1.into()));

    // Disk was NOT overwritten at seq 2 and did NOT receive seq 3.
    let disk = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap()
        .replay(session)
        .await
        .unwrap();
    assert_eq!(disk.len(), 2);
    let disk_proj = Projection::from_events(&disk);
    assert_eq!(
        disk_proj.session.title.as_deref(),
        Some("foreign-writer-title")
    );
}

#[tokio::test]
async fn open_stall_falls_back_to_memory_within_open_timeout() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("open-stall-fallback");
    let db = case.db_path("hya.db");
    case.policy.stall_open_ms.store(2_000, Ordering::Relaxed);
    let started = Instant::now();
    let store = SessionStore::connect_runtime_or_memory_with_timeouts(
        db.to_str().unwrap(),
        Duration::from_millis(80),
        Duration::from_millis(40),
        Duration::from_millis(40),
    )
    .await
    .expect("open timeout must fall back to memory store");
    let elapsed = started.elapsed();
    case.policy.stall_open_ms.store(0, Ordering::Relaxed);

    assert!(
        elapsed < Duration::from_millis(500),
        "open stall fallback exceeded bound: {elapsed:?}"
    );
    assert!(store.paths().is_none());
    let (fb_path, fb_reason) = store
        .memory_fallback()
        .expect("memory_fallback metadata must be populated");
    assert_eq!(fb_path, db);
    assert!(fb_reason.contains("timeout"), "{fb_reason}");

    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    assert_eq!(store.replay(session).await.unwrap().len(), 1);
}

#[tokio::test]
async fn open_unwritable_paths_fall_back_to_memory() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("open-unwritable-fallback");

    // 1. Missing parent directory -> NotFound -> memory fallback.
    let missing_db = case.dir.join("no-such-parent").join("hya.db");
    let mem1 = SessionStore::connect_runtime_or_memory(missing_db.to_str().unwrap())
        .await
        .unwrap();
    assert!(mem1.memory_fallback().is_some());
    assert!(mem1.paths().is_none());
    let s1 = SessionId::new();
    mem1.append_event(s1, &session_created_event(s1, &case.dir))
        .await
        .unwrap();
    assert_eq!(mem1.replay(s1).await.unwrap().len(), 1);

    // 2. Permission-denied directories (0500 read-only & 0000 unsearchable) when non-root.
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        use std::os::unix::fs::PermissionsExt as _;
        struct RestoreMode(PathBuf);
        impl Drop for RestoreMode {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
            }
        }
        for (name, mode) in [("ro-dir", 0o500), ("inaccessible-dir", 0o000)] {
            let dir = case.dir.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
            let _guard = RestoreMode(dir.clone());
            let db = dir.join("hya.db");
            let mem = SessionStore::connect_runtime_or_memory(db.to_str().unwrap())
                .await
                .unwrap();
            assert!(mem.memory_fallback().is_some(), "{name} should fall back");
            let s = SessionId::new();
            mem.append_event(s, &session_created_event(s, &case.dir))
                .await
                .unwrap();
            assert_eq!(mem.replay(s).await.unwrap().len(), 1);
        }
    }

    // 3. ENOSPC during DB creation -> storage fault -> memory fallback.
    let enospc_db = case.db_path("enospc.db");
    case.policy.enospc.store(true, Ordering::Relaxed);
    let mem3 = SessionStore::connect_runtime_or_memory(enospc_db.to_str().unwrap())
        .await
        .unwrap();
    case.policy.enospc.store(false, Ordering::Relaxed);
    assert!(mem3.memory_fallback().is_some());
    let s3 = SessionId::new();
    mem3.append_event(s3, &session_created_event(s3, &case.dir))
        .await
        .unwrap();
    assert_eq!(mem3.replay(s3).await.unwrap().len(), 1);
}

#[tokio::test]
async fn open_fallback_never_masks_busy_unsupported_or_symlink_lock() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("open-no-mask-fallback");

    // 1. Live runtime owner -> RuntimeOwnerBusy propagates (exit 75 contract).
    let busy_db = case.db_path("busy.db");
    let owner = SessionStore::connect_runtime(busy_db.to_str().unwrap())
        .await
        .unwrap();
    owner
        .claim_runtime_owner(hya_store::OwnerRunId::new())
        .unwrap();
    assert!(matches!(
        SessionStore::connect_runtime_or_memory(busy_db.to_str().unwrap()).await,
        Err(hya_store::StoreError::RuntimeOwnerBusy)
    ));
    drop(owner);

    // 2. Unsupported legacy database schema -> UnsupportedDatabase propagates.
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
        Err(hya_store::StoreError::UnsupportedDatabase)
    ));

    // 3. Symlink lock file & database URI -> fail closed with RuntimeOwnerLock.
    #[cfg(unix)]
    {
        let sym_db = case.dir.join("sym.db");
        let victim = case.dir.join("victim");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, format!("{}.runtime-owner.lock", sym_db.display()))
            .unwrap();
        assert!(matches!(
            SessionStore::connect_runtime_or_memory(sym_db.to_str().unwrap()).await,
            Err(hya_store::StoreError::RuntimeOwnerLock { .. })
        ));
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    }
    assert!(matches!(
        SessionStore::connect_runtime_or_memory(&format!("sqlite:{}", busy_db.display())).await,
        Err(hya_store::StoreError::RuntimeOwnerLock { .. })
    ));
}

#[tokio::test]
async fn exit_drain_clean_recovered_unpersisted_halted_and_memory_fallback() {
    let _gate = clock_gate().read().await;

    // 1. Healthy file store -> Clean, zero stderr output.
    {
        let case = FaultCase::new("drain-clean");
        let db = case.db_path("hya.db");
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
        let mut out = Vec::new();
        let report = store
            .drain_and_report_exit_to(Duration::from_secs(2), &mut out)
            .await;
        assert_eq!(report, ExitDrainReport::Clean);
        assert_eq!(report.format_stderr_line(), None);
        assert!(out.is_empty());
    }

    // 2. ENOSPC degraded then cleared -> Recovered with non-empty reason and stderr notice.
    {
        let case = FaultCase::new("drain-enospc-recovered");
        let db = case.db_path("hya.db");
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
        store.read_projection(session).await.unwrap();

        case.policy.enospc.store(true, Ordering::Relaxed);
        store
            .append_event(
                session,
                &Event::SessionTitled {
                    session,
                    title: "enospc-recovered".into(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
        case.policy.enospc.store(false, Ordering::Relaxed);

        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(store.health(), StoreHealth::Healthy) {
            assert!(Instant::now() < deadline, "did not recover from enospc");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut out = Vec::new();
        let report = store
            .drain_and_report_exit_to(Duration::from_secs(2), &mut out)
            .await;
        match &report {
            ExitDrainReport::Recovered { path, reason } => {
                assert_eq!(path, &db);
                assert!(!reason.is_empty());
            }
            other => panic!("expected Recovered, got {other:?}"),
        }
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("recovered"), "{text}");
        assert!(text.contains(&db.display().to_string()), "{text}");
    }

    // 3. Stalled WAL write released mid-drain -> drain_for_exit waits and reports Recovered.
    {
        let case = FaultCase::new("drain-stall-recovered");
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
        store.read_projection(session).await.unwrap();

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
        assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

        let policy = Arc::clone(&case.policy);
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            policy.hold_wal_writes(false);
        });
        let report = store.drain_for_exit(Duration::from_secs(2)).await;
        releaser.await.unwrap();
        match report {
            ExitDrainReport::Recovered { path, reason } => {
                assert_eq!(path, db);
                assert!(!reason.is_empty());
            }
            other => panic!("expected Recovered after release, got {other:?}"),
        }
    }

    // 4. Persistent ENOSPC with 2 queued events + 1 blob -> Unpersisted exact counts & bytes,
    //    and drain_and_report_exit_to emits at most one stderr line per store.
    {
        let case = FaultCase::new("drain-unpersisted");
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
        store.read_projection(session).await.unwrap();

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
        let report1 = store
            .drain_and_report_exit_to(Duration::from_millis(250), &mut out)
            .await;
        match &report1 {
            ExitDrainReport::Unpersisted {
                path,
                events,
                bytes,
                reason,
            } => {
                assert_eq!(path, &db);
                assert_eq!(*events, 2);
                assert_eq!(*bytes, expected_bytes as u64);
                assert!(!reason.is_empty());
            }
            other => panic!("expected Unpersisted, got {other:?}"),
        }
        let mut second_out = Vec::new();
        let report2 = store
            .drain_and_report_exit_to(Duration::from_millis(50), &mut second_out)
            .await;
        assert_eq!(report2, report1);
        assert!(second_out.is_empty(), "second report call must be silent");

        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains("unpersisted"), "{text}");
        assert!(text.contains("2 event(s)"), "{text}");
        assert!(text.contains(&format!("{expected_bytes} bytes")), "{text}");
        assert!(text.contains(&db.display().to_string()), "{text}");
    }

    // 5. Halted writer (file replaced) -> drain_for_exit(30s) returns immediately without waiting.
    {
        let case = FaultCase::new("drain-halted");
        let db = case.db_path("hya.db");
        let moved = case.db_path("hya.db.moved");
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
        store.read_projection(session).await.unwrap();

        std::fs::rename(&db, &moved).unwrap();
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
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "halted writer must not wait 30s: {elapsed:?}"
        );
        match report {
            ExitDrainReport::Unpersisted {
                path,
                events,
                reason,
                ..
            } => {
                assert_eq!(path, db);
                assert_eq!(events, 1);
                assert!(reason.contains("replaced"), "{reason}");
            }
            other => panic!("expected Unpersisted on halt, got {other:?}"),
        }
    }

    // 6. Open fallback to memory -> InMemoryFallback with exact events & payload+blob bytes.
    #[cfg(unix)]
    if unsafe { libc::geteuid() } != 0 {
        use std::os::unix::fs::PermissionsExt as _;
        struct RestoreMode(PathBuf);
        impl Drop for RestoreMode {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
            }
        }
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
        match &report {
            ExitDrainReport::InMemoryFallback {
                path,
                events,
                bytes,
                reason,
            } => {
                assert_eq!(path, &db);
                assert_eq!(*events, Some(2));
                assert_eq!(*bytes, Some(expected_bytes as u64));
                assert!(!reason.is_empty());
            }
            other => panic!("expected InMemoryFallback, got {other:?}"),
        }
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("nothing was persisted"), "{text}");
        assert!(text.contains("2 event(s)"), "{text}");
        assert!(text.contains(&format!("{expected_bytes} bytes")), "{text}");
        assert!(text.contains(&db.display().to_string()), "{text}");
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
        Duration::from_millis(80),
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
    let start_ev = Event::MessageStarted {
        session,
        message: msg,
        role: Role::Assistant,
        agent: Some(AgentName::new("hya")),
        model: Some(ModelRef::new("fake/model")),
    };
    let text_start = Event::TextStart {
        session,
        message: msg,
        part,
    };
    store_a
        .append_events(session, vec![start_ev.clone(), text_start.clone()])
        .await
        .unwrap();
    store_b
        .append_events(session, vec![start_ev, text_start])
        .await
        .unwrap();

    let mut blob_hashes = Vec::new();
    for step in 0..200_u32 {
        if step == 20 {
            fault_case.policy.hold_wal_writes(true);
            let pol = Arc::clone(&fault_case.policy);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(250)).await;
                pol.hold_wal_writes(false);
            });
        }
        if step == 45 {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !matches!(store_b.health(), StoreHealth::Healthy) {
                assert!(
                    Instant::now() < deadline,
                    "did not recover after write stall"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        if step == 80 {
            fault_case.policy.enospc.store(true, Ordering::Relaxed);
        }
        if step == 120 {
            fault_case.policy.enospc.store(false, Ordering::Relaxed);
        }

        let ev = Event::TextDelta {
            session,
            message: msg,
            part,
            delta: format!("[{step:03}]"),
        };
        if step.is_multiple_of(40) {
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

        if step == 30 || step == 100 {
            assert!(
                matches!(store_b.health(), StoreHealth::Degraded { .. }),
                "expected store_b degraded at step {step}"
            );
            let proj_a = store_a.read_projection_shared(session).await.unwrap();
            let proj_b = store_b.read_projection_shared(session).await.unwrap();
            assert_eq!(proj_a.session.messages.len(), proj_b.session.messages.len());
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
    match store_b.drain_for_exit(Duration::from_secs(5)).await {
        ExitDrainReport::Recovered { path, reason } => {
            assert_eq!(path, fault_db);
            assert!(!reason.is_empty());
        }
        other => panic!("expected Recovered after VFS write stall + ENOSPC, got {other:?}"),
    }

    let ro_a = SessionStore::connect_read_only(healthy_db.to_str().unwrap())
        .await
        .unwrap();
    let ro_b = SessionStore::connect_read_only(fault_db.to_str().unwrap())
        .await
        .unwrap();
    let disk_a = ro_a.replay(session).await.unwrap();
    let disk_b = ro_b.replay(session).await.unwrap();
    assert_eq!(disk_a.len(), 204);
    assert_eq!(disk_a.len(), disk_b.len());
    for (ea, eb) in disk_a.iter().zip(disk_b.iter()) {
        assert_eq!(ea.seq, eb.seq);
        assert_eq!(ea.event, eb.event);
    }
    let mut norm_a = disk_a.clone();
    let mut norm_b = disk_b.clone();
    for env in norm_a.iter_mut().chain(norm_b.iter_mut()) {
        env.ts_millis = 0;
    }
    assert_eq!(
        Projection::from_events(&norm_a),
        Projection::from_events(&norm_b)
    );
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
async fn cold_read_after_replacement_refuses_foreign_rows() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("cold-read-replacement");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "ours-committed".into(),
            },
        )
        .await
        .unwrap();
    // No read so far: the lazy read pool holds no connection yet.
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
    let health = store.health();
    eprintln!("health after identity change: {health:?}");
    assert!(matches!(health, StoreHealth::Degraded { .. }));

    // Queue-only read still answers without hitting disk.
    assert_eq!(
        store.file_blob(session, "qblob").await.unwrap().as_deref(),
        Some(b"queued-blob".as_slice())
    );

    // Every disk read must refuse with the halted error rather than folding foreign rows.
    let proj = store.read_projection(session).await;
    assert!(
        matches!(&proj, Err(StoreError::StoreDegraded { reason }) if reason.contains("replaced")),
        "cold read_projection must reject replacement file, got {:?}",
        proj.as_ref().map(|p| (&p.session.title, p.last_seq))
    );
    let replay = store.replay(session).await;
    assert!(
        matches!(&replay, Err(StoreError::StoreDegraded { reason }) if reason.contains("replaced")),
        "replay must reject replacement file, got {replay:?}"
    );
    let list = store.list_sessions().await;
    assert!(
        matches!(&list, Err(StoreError::StoreDegraded { reason }) if reason.contains("replaced")),
        "list_sessions must reject replacement file, got {list:?}"
    );
    let bytes = store.file_blob_bytes(session).await;
    assert!(
        matches!(&bytes, Err(StoreError::StoreDegraded { reason }) if reason.contains("replaced")),
        "file_blob_bytes must reject replacement file, got {bytes:?}"
    );
    let missing_blob = store.file_blob(session, "missing").await;
    assert!(
        matches!(&missing_blob, Err(StoreError::StoreDegraded { reason }) if reason.contains("replaced")),
        "uncached file_blob must reject replacement file, got {missing_blob:?}"
    );

    // Separate store where one session is warm in ProjectionCache before replacement:
    // the warm session still reads from cache while a cold session still refuses.
    let db2 = case.db_path("hya2.db");
    let store2 =
        SessionStore::connect_with_write_timeout(db2.to_str().unwrap(), Duration::from_millis(60))
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
    assert!(matches!(
        store2.read_projection(cold).await,
        Err(StoreError::StoreDegraded { .. })
    ));
}

#[tokio::test]
async fn raw_tx_after_replacement_halts_and_reports_unpersisted() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("raw-tx-replacement");
    let db = case.db_path("hya.db");
    let moved = case.db_path("hya.db.moved");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
            .await
            .unwrap();
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    std::fs::rename(&db, &moved).unwrap();

    // 1. Raw transaction after file leaves its path must refuse with StoreDegraded and never commit to the old inode.
    let tx_err = match store.begin_transaction().await {
        Ok(_tx) => panic!("begin_transaction must refuse after db left its path"),
        Err(err) => err,
    };
    eprintln!("begin_transaction err: {tx_err}");
    assert!(
        matches!(tx_err, StoreError::StoreDegraded { .. }),
        "expected StoreDegraded from begin_transaction, got {tx_err:?}"
    );
    let report = store.drain_for_exit(Duration::from_secs(1)).await;
    eprintln!(
        "report after refused raw tx: {report:?}; db exists: {}",
        db.exists()
    );
    assert!(
        matches!(
            report,
            ExitDrainReport::Unpersisted {
                events: 0,
                bytes: 0,
                ..
            }
        ),
        "drain_for_exit must report Unpersisted (not Clean) when file left its path, got {report:?}"
    );

    // 2. Same report when the file leaves its path after the last healthy append with no later writes.
    let db2 = case.db_path("hya2.db");
    let moved2 = case.db_path("hya2.db.moved");
    let store2 =
        SessionStore::connect_with_write_timeout(db2.to_str().unwrap(), Duration::from_millis(60))
            .await
            .unwrap();
    store2
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    std::fs::rename(&db2, &moved2).unwrap();
    let report2 = store2.drain_for_exit(Duration::from_secs(1)).await;
    eprintln!("idle move report: {report2:?}");
    assert!(
        matches!(
            report2,
            ExitDrainReport::Unpersisted {
                events: 0,
                bytes: 0,
                ..
            }
        ),
        "drain_for_exit must not return Clean when db moved after last write, got {report2:?}"
    );
}

#[tokio::test]
async fn transient_stat_error_degrades_and_recovers_on_restore() {
    use std::os::unix::fs::PermissionsExt as _;
    let _gate = clock_gate().read().await;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    struct RestorePerm(PathBuf);
    impl Drop for RestorePerm {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let case = FaultCase::new("transient-stat-recovers");
    let dir = case.dir.join("sub");
    std::fs::create_dir_all(&dir).unwrap();
    let _restore = RestorePerm(dir.clone());
    let db = dir.join("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
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
    eprintln!("health during/after EACCES: {:?}", store.health());
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
    let report = store.drain_for_exit(Duration::from_secs(2)).await;
    eprintln!("report after permission restored: {report:?}");
    assert!(
        matches!(report, ExitDrainReport::Recovered { .. }),
        "the file was intact and reachable again; the queue should have been flushed, got {report:?}"
    );
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.replay(session).await.unwrap().len(), 3);
}

#[tokio::test]
async fn degraded_session_exists_matches_queued_projection() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("degraded-session-exists");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(40))
            .await
            .unwrap();
    let warm = SessionId::new();
    store
        .append_event(warm, &session_created_event(warm, &case.dir))
        .await
        .unwrap();
    case.policy.hold_wal_writes(true);
    let session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
    let proj = store.read_projection(session).await.unwrap();
    let exists = store.session_exists(session).await.unwrap();
    let info = store.session_info(session).await.unwrap();
    case.policy.hold_wal_writes(false);
    eprintln!(
        "projection.id={:?} session_exists={exists} session_info={info:?}",
        proj.session.id
    );
    assert_eq!(proj.session.id, Some(session));
    assert!(
        exists,
        "session_exists answered from the disk alone while its creation was acknowledged and queued"
    );
    assert!(
        !store.session_exists(SessionId::new()).await.unwrap(),
        "absent session must still report false"
    );
}

#[tokio::test]
async fn clean_exit_report_does_not_consume_stderr_latch() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("clean-exit-report-latch");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(40))
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
    assert!(first.is_empty());

    case.policy.enospc.store(true, Ordering::Relaxed);
    store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "after-first-report".into(),
            },
        )
        .await
        .unwrap();
    let mut second = Vec::new();
    let r2 = store
        .drain_and_report_exit_to(Duration::from_millis(200), &mut second)
        .await;
    case.policy.enospc.store(false, Ordering::Relaxed);
    eprintln!(
        "second report {r2:?}, printed {:?}",
        String::from_utf8_lossy(&second)
    );
    assert!(matches!(r2, ExitDrainReport::Unpersisted { .. }));
    assert!(
        !second.is_empty(),
        "unpersisted events at the reported exit, but no line was printed"
    );
}

async fn assert_pk_conflict_halts_with_foreign_row(
    label: &str,
    bind_different_session: bool,
    ts_offset: i64,
) {
    let case = FaultCase::new(label);
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(50))
            .await
            .unwrap();
    let session = SessionId::new();
    let other_session = SessionId::new();
    store
        .append_event(session, &session_created_event(session, &case.dir))
        .await
        .unwrap();
    if bind_different_session {
        store
            .append_event(
                other_session,
                &session_created_event(other_session, &case.dir),
            )
            .await
            .unwrap();
    }

    let mut external = sqlx::SqliteConnection::connect(&format!("sqlite://{}", db.display()))
        .await
        .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut external)
        .await
        .unwrap();

    let ours = Event::SessionTitled {
        session,
        title: "same-payload".into(),
    };
    let our_payload = serde_json::to_string(&ours).unwrap();
    let envelopes = store.append_events(session, vec![ours]).await.unwrap();
    let conflict_seq = i64::try_from(envelopes[0].seq.0).unwrap();
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));

    let disk_session = if bind_different_session {
        other_session
    } else {
        session
    };
    sqlx::query("INSERT INTO event_log (seq, session_id, payload, ts) VALUES (?, ?, ?, ?)")
        .bind(conflict_seq)
        .bind(disk_session.storage_key())
        .bind(&our_payload)
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
        "{label}: expected foreign writer conflict halt with 1 unpersisted event, got {report:?}"
    );
}

#[tokio::test]
async fn pk_conflict_different_session_same_payload_halts_writer() {
    let _gate = clock_gate().read().await;
    assert_pk_conflict_halts_with_foreign_row("pk-conflict-diff-session", true, 0).await;
}

#[tokio::test]
async fn pk_conflict_different_timestamp_same_payload_halts_writer() {
    let _gate = clock_gate().read().await;
    assert_pk_conflict_halts_with_foreign_row("pk-conflict-diff-ts", false, 1).await;
}

#[tokio::test]
async fn transient_stat_error_before_raw_tx_does_not_stick_store_degraded() {
    use std::os::unix::fs::PermissionsExt as _;
    let _gate = clock_gate().read().await;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    struct RestorePerm(PathBuf);
    impl Drop for RestorePerm {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let case = FaultCase::new("transient-stat-raw-tx");
    let dir = case.dir.join("sub");
    std::fs::create_dir_all(&dir).unwrap();
    let _restore = RestorePerm(dir.clone());
    let db = dir.join("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
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

    tokio::time::sleep(Duration::from_millis(200)).await;
    let health_after = store.health();
    let mut after = Vec::new();
    for _ in 0..3 {
        after.push(store.begin_transaction().await.map(drop));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        matches!(during, Err(StoreError::StoreDegraded { .. })),
        "raw tx during transient stat error must fail: {during:?}"
    );
    assert!(
        matches!(health_after, StoreHealth::Healthy),
        "queue must stay Healthy after transient stat error before raw tx: {health_after:?}"
    );
    assert!(
        after.iter().all(|r| r.is_ok()),
        "raw transactions must succeed once file access returns without any intervening append: {after:?}"
    );
}

#[tokio::test]
async fn warm_session_exists_answers_from_cache_after_identity_halt() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("warm-session-exists-cache");
    let db = case.db_path("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
            .await
            .unwrap();
    let warm = SessionId::new();
    let cold = SessionId::new();
    store
        .append_event(warm, &session_created_event(warm, &case.dir))
        .await
        .unwrap();
    assert_eq!(
        store.read_projection(warm).await.unwrap().session.id,
        Some(warm)
    );

    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }

    assert_eq!(
        store.read_projection(warm).await.unwrap().session.id,
        Some(warm)
    );
    assert!(
        store.session_exists(warm).await.unwrap(),
        "warm cached session_exists must answer true from ProjectionCache after identity halt"
    );
    assert!(
        matches!(
            store.session_exists(cold).await,
            Err(StoreError::StoreDegraded { .. })
        ),
        "uncached session_exists must still refuse with StoreDegraded after identity halt"
    );
}

#[tokio::test]
async fn transient_stat_error_at_exit_with_empty_queue_reports_clean() {
    use std::os::unix::fs::PermissionsExt as _;
    let _gate = clock_gate().read().await;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    struct RestorePerm(PathBuf);
    impl Drop for RestorePerm {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    let case = FaultCase::new("transient-stat-exit-empty");
    let dir = case.dir.join("sub");
    std::fs::create_dir_all(&dir).unwrap();
    let _restore = RestorePerm(dir.clone());
    let db = dir.join("hya.db");
    let store =
        SessionStore::connect_with_write_timeout(db.to_str().unwrap(), Duration::from_millis(60))
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
    assert!(
        buf.is_empty(),
        "transient stat error with empty queue must emit no unpersisted banner, got: {:?}",
        String::from_utf8_lossy(&buf)
    );
    drop(store);
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.replay(session).await.unwrap().len(), 1);
}
