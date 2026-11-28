use std::path::Path;

use super::*;
use crate::connect::{TEST_STALL_OPEN, is_open_storage_fault};

#[tokio::test]
async fn memory_spellings_keep_data_across_pool_connections() {
    for path in ["", ":memory:"] {
        for store in [
            SessionStore::connect(path).await.unwrap(),
            SessionStore::connect_runtime(path).await.unwrap(),
            SessionStore::connect_read_only(path).await.unwrap(),
        ] {
            let session = SessionId::new();
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
            for _ in 0..3 {
                store
                    .writer_pool
                    .acquire()
                    .await
                    .unwrap()
                    .close()
                    .await
                    .unwrap();
                assert_eq!(store.replay(session).await.unwrap().len(), 1);
            }
        }
    }
}

#[tokio::test]
async fn tree_replay_merges_persisted_and_queued_descendants()
-> Result<(), Box<dyn std::error::Error>> {
    let _guard = file_store_test_lock().await;
    let dir = std::env::temp_dir().join(format!("hya-tree-{}", SessionId::new()));
    std::fs::create_dir(&dir)?;
    let db = dir.join("tree.db");
    let store = SessionStore::connect_with_timeouts(
        db.to_str().ok_or("invalid db path")?,
        Duration::from_millis(100),
        Duration::from_secs(2),
    )
    .await?;
    let (root, child, grandchild, other) = (
        SessionId::new(),
        SessionId::new(),
        SessionId::new(),
        SessionId::new(),
    );
    let created = |session, parent| Event::SessionCreated {
        session,
        parent,
        agent: "build".into(),
        model: "fake".into(),
        workdir: ".".into(),
        project: None,
        kind: hya_proto::SessionKind::Project,
    };
    store.append_event(root, &created(root, None)).await?;
    store.append_event(other, &created(other, None)).await?;
    let held = store.writer_pool.begin_with("BEGIN IMMEDIATE").await?;
    let queued_root = SessionId::new();
    store
        .append_event(queued_root, &created(queued_root, None))
        .await?;
    for (session, parent) in [
        (child, root),
        (grandchild, child),
        (other, root),
        (queued_root, child),
    ] {
        store
            .append_event(session, &created(session, Some(parent)))
            .await?;
    }
    store
        .append_event(
            other,
            &Event::SessionTitled {
                session: other,
                title: "elsewhere".into(),
            },
        )
        .await?;
    assert!(matches!(store.health(), StoreHealth::Degraded { .. }));
    let queued = store.replay_tree(root).await?;
    assert_eq!(
        queued
            .iter()
            .filter_map(|env| env.event.session())
            .collect::<Vec<_>>(),
        vec![root, child, grandchild]
    );
    assert!(queued.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    held.rollback().await?;
    let report = store.drain_for_exit(Duration::from_secs(5)).await;
    assert!(matches!(report, ExitDrainReport::Recovered { .. }));
    assert_eq!(
        serde_json::to_value(store.replay_tree(root).await?)?,
        serde_json::to_value(queued)?
    );
    drop(store);
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

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
        .execute(&store.writer_pool)
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
                .execute(&store.writer_pool)
                .await
                .unwrap();
        }
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
        sqlx::query(sql).execute(&store.writer_pool).await.unwrap();
    }
    // A late failure must roll back the event and auxiliary deletes too.
    sqlx::query("CREATE TRIGGER reject_delete BEFORE DELETE ON session BEGIN SELECT RAISE(ABORT, 'delete blocked'); END")
        .execute(&store.writer_pool).await.unwrap();
    assert!(store.delete_session(root).await.is_err());
    assert_eq!(store.replay(root).await.unwrap().len(), 2);
    assert!(store.file_blob(child, "hash").await.unwrap().is_some());
    sqlx::query("DROP TRIGGER reject_delete")
        .execute(&store.writer_pool)
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
    ] {
        for session in [root, child] {
            let count: i64 =
                sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?"))
                    .bind(session.storage_key())
                    .fetch_one(&store.read_pool)
                    .await
                    .unwrap();
            assert_eq!(count, 0, "{table} retains {session}");
        }
        let count: i64 =
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?"))
                .bind(other.storage_key())
                .fetch_one(&store.read_pool)
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
            .fetch_one(&store.read_pool)
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
        .execute(&store.writer_pool)
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
        .execute(&store.writer_pool)
        .await
        .unwrap();
    let error = store.list_sessions().await.unwrap_err();
    assert!(matches!(error, StoreError::SessionKeyData(_)), "{error}");
    assert!(error.to_string().contains("hysec_"), "{error}");
    assert!(decode_session_key(&[0xff; 16]).is_err());
}

#[tokio::test]
async fn sqlite_pools_disable_lifetime_and_idle_retirement() {
    let _guard = file_store_test_lock().await;
    let dir = std::env::temp_dir().join(format!(
        "hya-store-pool-opts-{}-{}",
        std::process::id(),
        now_millis()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("sessions.db");
    let rw = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
    assert_eq!(rw.writer_pool.options().get_max_connections(), 1);
    assert!(!rw.writer_pool.options().get_test_before_acquire());
    assert_eq!(rw.writer_pool.options().get_max_lifetime(), None);
    assert_eq!(rw.writer_pool.options().get_idle_timeout(), None);
    assert_eq!(rw.read_pool.options().get_max_lifetime(), None);
    assert_eq!(rw.read_pool.options().get_idle_timeout(), None);

    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(ro.read_pool.options().get_max_lifetime(), None);
    assert_eq!(ro.read_pool.options().get_idle_timeout(), None);

    let mem = SessionStore::connect_memory().await.unwrap();
    assert!(!mem.writer_pool.options().get_test_before_acquire());
    assert_eq!(mem.writer_pool.options().get_max_lifetime(), None);
    assert_eq!(mem.writer_pool.options().get_idle_timeout(), None);

    ro.read_pool.close().await;
    rw.writer_pool.close().await;
    rw.read_pool.close().await;
    drop(ro);
    drop(rw);
    drop(mem);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn snapshot_persist_skips_without_waiting_when_writer_is_busy() {
    let _guard = file_store_test_lock().await;
    let dir = std::env::temp_dir().join(format!(
        "hya-store-snapshot-try-{}-{}",
        std::process::id(),
        now_millis()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("sessions.db");
    let store = SessionStore::connect(db.to_str().unwrap())
        .await
        .unwrap()
        .with_projection_snapshot_interval(1);
    let session = SessionId::new();
    store
        .append_event(
            session,
            &Event::SessionCreated {
                session,
                parent: None,
                agent: hya_proto::AgentName::new("build"),
                model: hya_proto::ModelRef::new("fake"),
                workdir: "/tmp".into(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            },
        )
        .await
        .unwrap();

    let tx = store.begin_transaction().await.unwrap();
    let started = std::time::Instant::now();
    for _ in 0..3 {
        let proj = store.read_projection(session).await.unwrap();
        assert_eq!(proj.last_seq, 1);
    }
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "busy writer must not delay snapshot reads (elapsed={:?})",
        started.elapsed()
    );
    let count_busy: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projection_snapshot")
        .fetch_one(&store.read_pool)
        .await
        .unwrap();
    assert_eq!(count_busy, 0);

    drop(tx);
    tokio::time::timeout(Duration::from_secs(2), async {
        while store.writer_pool.num_idle() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("writer connection returns to idle");

    let proj = store.read_projection(session).await.unwrap();
    assert_eq!(proj.last_seq, 1);
    let count_idle: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projection_snapshot")
        .fetch_one(&store.read_pool)
        .await
        .unwrap();
    assert_eq!(count_idle, 1);

    store.writer_pool.close().await;
    store.read_pool.close().await;
    drop(store);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn writer_and_read_pools_are_strictly_separated() {
    let _guard = file_store_test_lock().await;
    let dir = std::env::temp_dir().join(format!(
        "hya-store-pool-split-{}-{}",
        std::process::id(),
        now_millis()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("sessions.db");
    let store = SessionStore::connect(db.to_str().unwrap()).await.unwrap();
    assert_eq!(store.writer_pool.options().get_max_connections(), 1);
    assert_eq!(store.read_pool.options().get_max_connections(), 4);
    assert!(!store.writer_pool.options().get_test_before_acquire());

    let session = SessionId::new();
    store
        .append_event(
            session,
            &Event::SessionCreated {
                session,
                parent: None,
                agent: hya_proto::AgentName::new("build"),
                model: hya_proto::ModelRef::new("fake"),
                workdir: "/tmp".into(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        store.read_pool.size(),
        0,
        "writes must never touch the lazy read pool"
    );
    assert_eq!(store.writer_pool.size(), 1);

    let tx = store.begin_transaction().await.unwrap();
    assert!(
        store.writer_pool.try_acquire().is_none(),
        "sole writer connection must be occupied while tx is held"
    );
    let deadline = Duration::from_millis(500);
    assert_eq!(
        tokio::time::timeout(deadline, store.replay(session))
            .await
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        tokio::time::timeout(deadline, store.list_sessions())
            .await
            .unwrap()
            .unwrap()
            .len(),
        1
    );
    assert!(
        tokio::time::timeout(deadline, store.session_exists(session))
            .await
            .unwrap()
            .unwrap()
    );
    assert_eq!(
        tokio::time::timeout(deadline, store.read_projection(session))
            .await
            .unwrap()
            .unwrap()
            .last_seq,
        1
    );
    assert!(
        store.read_pool.size() >= 1,
        "reads must execute on read_pool"
    );
    let ro_write_err =
        sqlx::query("INSERT INTO event_log (session_id, ts, payload) VALUES (?, 1, '{}')")
            .bind(session.storage_key())
            .execute(&store.read_pool)
            .await
            .unwrap_err();
    assert!(
        ro_write_err.to_string().contains("readonly"),
        "read_pool must reject writes: {ro_write_err}"
    );

    tx.rollback().await.unwrap();
    store.writer_pool.close().await;
    store.read_pool.close().await;
    drop(store);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn migration_script_storage_fault_is_classified() {
    let io = sqlx::Error::Io(std::io::Error::other("injected EIO"));
    let err = StoreError::from(sqlx::migrate::MigrateError::ExecuteMigration(io, 1));
    assert!(is_open_storage_fault(&err), "{err}");
}

#[test]
fn is_open_storage_fault_classifies_migration_arms() {
    let io_exec = StoreError::from(sqlx::migrate::MigrateError::Execute(sqlx::Error::Io(
        std::io::Error::other("injected EIO"),
    )));
    let io_script = StoreError::from(sqlx::migrate::MigrateError::ExecuteMigration(
        sqlx::Error::Io(std::io::Error::other("injected script EIO")),
        1,
    ));
    let non_storage_exec = StoreError::from(sqlx::migrate::MigrateError::Execute(
        sqlx::Error::Protocol("bad migration SQL".into()),
    ));
    let non_storage_script = StoreError::from(sqlx::migrate::MigrateError::ExecuteMigration(
        sqlx::Error::Protocol("bad script SQL".into()),
        1,
    ));
    let missing = StoreError::from(sqlx::migrate::MigrateError::VersionMissing(1));

    assert!(is_open_storage_fault(&io_exec), "{io_exec}");
    assert!(is_open_storage_fault(&io_script), "{io_script}");
    assert!(!is_open_storage_fault(&non_storage_exec));
    assert!(!is_open_storage_fault(&non_storage_script));
    assert!(!is_open_storage_fault(&missing));
}

#[test]
fn stalled_filesystem_open_times_out_and_exits_runtime_promptly() {
    if std::env::var_os("HYA_TEST_STALLED_OPEN_CHILD").is_none() {
        let started = std::time::Instant::now();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "event_contract_tests::stalled_filesystem_open_times_out_and_exits_runtime_promptly",
                "--nocapture",
            ])
            .env("HYA_TEST_STALLED_OPEN_CHILD", "1")
            .output()
            .unwrap();
        let elapsed = started.elapsed();
        assert!(
            output.status.success(),
            "stalled open child failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            elapsed < Duration::from_secs(4),
            "process exit hung waiting on stalled open thread ({elapsed:?})"
        );
        return;
    }

    TEST_STALL_OPEN.store(true, std::sync::atomic::Ordering::Release);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let bound = Duration::from_millis(120);
    rt.block_on(async {
        let strict_res = SessionStore::connect_runtime_bounded(
            "/tmp/stalled-strict.db",
            bound,
            WRITE_TIMEOUT,
            READ_TIMEOUT,
        )
        .await;
        assert!(matches!(
            strict_res,
            Err((
                StoreError::StorageTimeout {
                    operation: "open",
                    ..
                },
                _
            ))
        ));

        let fallback = SessionStore::connect_runtime_or_memory_with_timeouts(
            "/tmp/stalled-fallback.db",
            bound,
            WRITE_TIMEOUT,
            READ_TIMEOUT,
        )
        .await
        .unwrap();
        let (path, reason) = fallback.memory_fallback().unwrap();
        assert_eq!(path, Path::new("/tmp/stalled-fallback.db"));
        assert!(reason.contains("timeout"), "{reason}");
    });
    drop(rt);
    TEST_STALL_OPEN.store(false, std::sync::atomic::Ordering::Release);
}
