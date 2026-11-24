//! Registry fault tests run in their own process because the SQLite VFS and
//! Linux monotonic clock hooks affect every test in the process.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_arguments)]

use std::str::FromStr;

use hya_app::bundle_registry::BundleRegistry;
use sqlx::Error as SqlxError;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

include!("../../hya-store/tests/support/fault_vfs.rs");

fn db_error_details(err: &SqlxError) -> Option<(String, String)> {
    if let SqlxError::Database(db) = err {
        Some((
            db.code().map_or_else(|| "none".into(), |c| c.into_owned()),
            db.message().to_string(),
        ))
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn bundle_registry_does_not_retire_connection_after_max_lifetime() {
    let _gate = clock_gate().write().await;
    let case = FaultCase::new("bundle-no-max-lifetime-retire");
    let path = case.db_path("registry.sqlite3");
    let registry = BundleRegistry::connect(path.to_str().unwrap())
        .await
        .unwrap();
    registry.generation().await.unwrap();

    advance_monotonic_secs(1830);
    case.policy.take_trace();
    registry.generation().await.unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;
    let trace = case.policy.take_trace();
    assert!(
        !trace
            .iter()
            .any(|e| (e.kind == FileKind::MainDb && e.op == "xClose")
                || (e.kind == FileKind::Wal && e.op == "xDelete")),
        "bundle registry retired a connection: {trace:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bundle_registry_set_bundle_enabled_prevents_busy_snapshot() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("bundle-no-busy-snapshot");
    let reg_path = case.db_path("registry.sqlite3");
    let reg = BundleRegistry::connect(reg_path.to_str().unwrap())
        .await
        .unwrap();

    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", reg_path.display()))
        .unwrap()
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(2));
    let helper_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();

    // Invalidate any warm pcache on `reg`'s pooled connection by committing once
    // from `helper_pool` first, so the next SELECT inside `set_bundle_enabled`
    // must read via VFS `xRead` after locking its read snapshot.
    {
        let mut htx = helper_pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query(
            "UPDATE bundle_registry_generation SET generation = generation + 1 WHERE singleton = 1",
        )
        .execute(&mut *htx)
        .await
        .unwrap();
        htx.commit().await.unwrap();
    }

    let (trig_tx, trig_rx) = tokio::sync::oneshot::channel::<()>();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let hp = helper_pool.clone();
    let racer = tokio::spawn(async move {
        if trig_rx.await.is_err() {
            return;
        }
        let mut htx = hp.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query(
            "UPDATE bundle_registry_generation SET generation = generation + 1 WHERE singleton = 1",
        )
        .execute(&mut *htx)
        .await
        .unwrap();
        htx.commit().await.unwrap();
        let _ = done_tx.send(());
    });

    // Skip the single initial page-1 header check inside `lockBtree()` so the
    // hook only fires if the transaction queries table pages *without* holding
    // `WAL_WRITE_LOCK` (deferred `BEGIN`). With `BEGIN IMMEDIATE`, the write
    // lock is acquired immediately after `lockBtree()` and before `SELECT`.
    // Note: this relies on SQLite reading one page before acquiring
    // `WAL_WRITE_LOCK` and routing table reads through `xRead` (non-mmap); a
    // SQLite change fails loudly via `hook_unfired` or the negative control.
    case.policy.arm_shm_read_hook(1, move || {
        let _ = trig_tx.send(());
        let _ = done_rx.recv();
    });

    case.policy.take_trace();
    let outcome = reg.set_bundle_enabled("acme/demo", false).await;
    let hook_unfired = case.policy.clear_shm_read_hook();
    let trace = case.policy.take_trace();
    drop(racer.await);
    outcome.unwrap_or_else(|e| {
        panic!("set_bundle_enabled must use BEGIN IMMEDIATE and avoid SQLITE_BUSY_SNAPSHOT (517): {e}; trace={trace:?}")
    });
    assert!(
        hook_unfired,
        "WAL_WRITE_LOCK was held before querying table data (BEGIN IMMEDIATE)"
    );
    assert!(
        trace.iter().any(|e| {
            e.kind == FileKind::MainDb
                && e.op == "xShmLock"
                && e.offset == 0
                && e.arg == i64::from(SQLITE_SHM_LOCK_EXCL)
                && e.rc == SQLITE_OK
        }),
        "expected BEGIN IMMEDIATE to acquire WAL_WRITE_LOCK: {trace:?}"
    );
    assert!(
        reg.disabled_bundle_ids()
            .await
            .unwrap()
            .contains("acme/demo")
    );

    // Negative control: with the exact same `skip_reads=1` hook, a deferred
    // `.begin()` read-before-write transaction still trips `SQLITE_BUSY_SNAPSHOT` (517).
    let opts_deferred = SqliteConnectOptions::from_str(&format!("sqlite://{}", reg_path.display()))
        .unwrap()
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(2));
    let deferred_pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts_deferred)
        .await
        .unwrap();
    let _: i64 =
        sqlx::query_scalar("SELECT generation FROM bundle_registry_generation WHERE singleton = 1")
            .fetch_one(&deferred_pool)
            .await
            .unwrap();
    {
        let mut htx = helper_pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query(
            "UPDATE bundle_registry_generation SET generation = generation + 1 WHERE singleton = 1",
        )
        .execute(&mut *htx)
        .await
        .unwrap();
        htx.commit().await.unwrap();
    }
    let (ctrl_trig_tx, ctrl_trig_rx) = tokio::sync::oneshot::channel::<()>();
    let (ctrl_done_tx, ctrl_done_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let ctrl_hp = helper_pool.clone();
    let ctrl_racer = tokio::spawn(async move {
        if ctrl_trig_rx.await.is_err() {
            return;
        }
        let mut htx = ctrl_hp.begin_with("BEGIN IMMEDIATE").await.unwrap();
        sqlx::query(
            "UPDATE bundle_registry_generation SET generation = generation + 1 WHERE singleton = 1",
        )
        .execute(&mut *htx)
        .await
        .unwrap();
        htx.commit().await.unwrap();
        let _ = ctrl_done_tx.send(());
    });
    case.policy.arm_shm_read_hook(1, move || {
        let _ = ctrl_trig_tx.send(());
        // Racer writes via a separate connection and cannot block on this one.
        let _ = ctrl_done_rx.recv();
    });
    let mut dtx = deferred_pool.begin().await.unwrap();
    let _: i64 =
        sqlx::query_scalar("SELECT generation FROM bundle_registry_generation WHERE singleton = 1")
            .fetch_one(&mut *dtx)
            .await
            .unwrap();
    let hook_fired = !case.policy.clear_shm_read_hook();
    assert!(hook_fired, "deferred SELECT should trigger the xRead hook");
    let ctrl_err = sqlx::query(
        "UPDATE bundle_registry_generation SET generation = generation + 1 WHERE singleton = 1",
    )
    .execute(&mut *dtx)
    .await
    .expect_err("deferred upgrade after concurrent write must fail with SQLITE_BUSY_SNAPSHOT");
    drop(dtx);
    drop(ctrl_racer.await);
    let (code, _) = db_error_details(&ctrl_err).unwrap();
    assert_eq!(code, "517", "unexpected deferred error: {ctrl_err}");
}

#[tokio::test]
#[ignore = "slow storage fault matrix; run with --ignored"]
async fn slow_bundle_registry_open_survives_32s_stall() {
    let _gate = clock_gate().read().await;
    let case = FaultCase::new("bundle-registry-open-32s");
    let reg_path = case.db_path("registry.sqlite3");
    drop(
        BundleRegistry::connect(reg_path.to_str().unwrap())
            .await
            .unwrap(),
    );
    case.policy.stall_open_ms.store(32_000, Ordering::Relaxed);
    let pol = Arc::clone(&case.policy);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        pol.stall_open_ms.store(0, Ordering::Relaxed);
    });
    let t0 = Instant::now();
    let reg = BundleRegistry::connect(reg_path.to_str().unwrap())
        .await
        .expect("BundleRegistry::connect should wait past sqlx default 30s acquire_timeout");
    assert!(
        t0.elapsed() >= Duration::from_secs(30),
        "elapsed={:?}",
        t0.elapsed()
    );
    assert_eq!(reg.generation().await.unwrap(), 0);
}
