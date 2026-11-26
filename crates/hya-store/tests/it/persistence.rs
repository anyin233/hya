//! Integration tests for `hya-store`: persistence, concurrency, usage, permissions, and reader pool.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::{Event, ModelRef, Projection, Role, SessionId, TokenUsage, UsagePurpose};
use hya_store::{SavedPermission, SessionStore};

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
                    "hya-persist-{nanos}-{}-{id}.db",
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

#[tokio::test]
async fn append_timestamp_and_reconnect_resume_sessions_and_hysec_usage() {
    let db = TempDb::new();
    let session = SessionId::new();
    let hysec: SessionId = "hysec_ABCDEFGHIJKLMNOPQRST".parse().unwrap();
    let (seq, ts) = {
        let store = SessionStore::connect(&db.0).await.unwrap();
        let (seq, ts) = store
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
        store
            .append_event(
                session,
                &Event::MessageStarted {
                    session,
                    message: hya_proto::MessageId::new(),
                    role: Role::Assistant,
                    agent: None,
                    model: None,
                },
            )
            .await
            .unwrap();
        for (model, purpose) in [
            ("12th/glm-5.3", UsagePurpose::Turn),
            ("12th/glm-5.3", UsagePurpose::Turn),
            ("title-model", UsagePurpose::Title),
        ] {
            store
                .append_event(
                    hysec,
                    &Event::UsageRecorded {
                        session: hysec,
                        message: None,
                        step: None,
                        model: ModelRef::new(model),
                        purpose,
                        tokens: TokenUsage {
                            input: 100,
                            output: 50,
                            ..Default::default()
                        },
                    },
                )
                .await
                .unwrap();
        }
        (seq, ts)
    };

    let reopened = SessionStore::connect(&db.0).await.unwrap();
    let envelopes = reopened.replay(session).await.unwrap();
    assert_eq!(envelopes.len(), 2);
    assert_eq!((envelopes[0].seq, envelopes[0].ts_millis), (seq, ts));
    let proj = reopened.read_projection(session).await.unwrap();
    assert_eq!(Projection::from_events(&envelopes), proj);
    assert_eq!(proj.session.id, Some(session));

    let hysec_proj = reopened.read_projection(hysec).await.unwrap();
    let usage = &hysec_proj.session.usage;
    assert_eq!(usage.by_model.len(), 2);
    let turns = usage.by_model[&ModelRef::new("12th/glm-5.3")];
    assert_eq!((turns.input, turns.output, turns.rounds), (200, 100, 2));
    assert_eq!(usage.by_purpose[&UsagePurpose::Turn], turns);
    assert_eq!(usage.total().input, 300);
    assert_eq!(
        hysec_proj,
        Projection::from_events(&reopened.replay(hysec).await.unwrap())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_do_not_lock() {
    let db = TempDb::new();
    let store = Arc::new(SessionStore::connect(&db.0).await.unwrap());
    let mut handles = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let session = SessionId::new();
            for i in 0..10 {
                store
                    .append_event(
                        session,
                        &Event::SessionTitled {
                            session,
                            title: format!("t{i}"),
                        },
                    )
                    .await
                    .unwrap();
            }
            session
        }));
    }
    for handle in handles {
        let session = handle.await.unwrap();
        let envelopes = store.replay(session).await.unwrap();
        assert_eq!(envelopes.len(), 10);
        for w in envelopes.windows(2) {
            assert!(w[0].seq.0 < w[1].seq.0);
        }
    }
}

#[tokio::test]
async fn saved_permissions_persist_across_reconnect_and_stamp_missing_time() {
    let db = TempDb::new();
    let stamped = SavedPermission {
        id: "psv_per_1".to_string(),
        project_id: "global".to_string(),
        action: "bash".to_string(),
        resource: "*".to_string(),
        time_created_ms: Some(1_700_000_000_000),
    };
    let unstamped = SavedPermission {
        id: "psv_per_2".to_string(),
        project_id: "global".to_string(),
        action: "tool".to_string(),
        resource: "write".to_string(),
        time_created_ms: None,
    };
    {
        let store = SessionStore::connect(&db.0).await.unwrap();
        store.save_permission(&stamped).await.unwrap();
        store.save_permission(&unstamped).await.unwrap();
    }

    let reopened = SessionStore::connect(&db.0).await.unwrap();
    assert_eq!(
        reopened.saved_permission("psv_per_1").await.unwrap(),
        Some(stamped)
    );
    let saved_auto = reopened
        .saved_permission("psv_per_2")
        .await
        .unwrap()
        .unwrap();
    assert!(
        saved_auto
            .time_created_ms
            .is_some_and(|ms| ms > 1_600_000_000_000)
    );

    reopened.remove_saved_permission("psv_per_1").await.unwrap();
    reopened.remove_saved_permission("psv_per_2").await.unwrap();
    let reopened = SessionStore::connect(&db.0).await.unwrap();
    assert!(
        reopened
            .list_saved_permissions(None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn read_pool_reads_do_not_block_on_open_write_transaction() {
    let db = TempDb::new();
    let store = SessionStore::connect(&db.0).await.unwrap();
    let session = SessionId::new();
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

    let tx = store.begin_transaction().await.unwrap();
    let deadline = std::time::Duration::from_millis(500);
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
    drop(tx);
}
