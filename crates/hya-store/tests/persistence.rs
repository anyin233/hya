//! Integration tests for `hya-store`: persistence.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::{Event, Projection, Role, SessionId};
use hya_store::{LedgerEntry, SavedPermission, SessionStore};

fn temp_db() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "hya-persist-{nanos}-{}-{id}.db",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn append_event_returns_the_persisted_timestamp() {
    let store = SessionStore::connect_memory().await.unwrap();
    let session = SessionId::new();
    let (seq, ts) = store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: "t".into(),
            },
        )
        .await
        .unwrap();
    let envelopes = store.replay(session).await.unwrap();
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].seq, seq);
    assert_eq!(
        envelopes[0].ts_millis, ts,
        "the bus-published envelope must carry the durable timestamp"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_do_not_lock() {
    let store = Arc::new(SessionStore::connect(&temp_db()).await.unwrap());
    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            let session = SessionId::new();
            for i in 0..20 {
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
        assert_eq!(envelopes.len(), 20);
        for w in envelopes.windows(2) {
            assert!(w[0].seq.0 < w[1].seq.0, "per-session order must hold");
        }
    }
}

#[tokio::test]
async fn session_resumes_after_reconnect() {
    let path = temp_db();
    let session = SessionId::new();
    {
        let store = SessionStore::connect(&path).await.unwrap();
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
        let m = hya_proto::MessageId::new();
        store
            .append_event(
                session,
                &Event::MessageStarted {
                    session,
                    message: m,
                    role: Role::Assistant,
                    agent: None,
                    model: None,
                },
            )
            .await
            .unwrap();
    }

    let reopened = SessionStore::connect(&path).await.unwrap();
    let envelopes = reopened.replay(session).await.unwrap();
    assert_eq!(envelopes.len(), 2);
    let projection = reopened.read_projection(session).await.unwrap();
    assert_eq!(Projection::from_events(&envelopes), projection);
    assert_eq!(projection.session.id, Some(session));
}

#[tokio::test]
async fn token_ledger_records_and_reads_by_role() {
    let store = SessionStore::connect_memory().await.unwrap();
    let session = SessionId::new();
    for (role, iteration) in [("worker", 1), ("verifier", 1), ("planner", 1)] {
        store
            .record_usage(&LedgerEntry {
                session,
                role: role.to_string(),
                iteration: Some(iteration),
                completion_run_id: None,
                prompt_tokens: 100,
                completion_tokens: 50,
                confidence: "actual".to_string(),
                provider: Some("12th".to_string()),
                model: Some("12th/glm-5.3".to_string()),
            })
            .await
            .unwrap();
    }
    let entries = store.read_usage(session).await.unwrap();
    assert_eq!(entries.len(), 3);
    assert!(entries.iter().any(|e| e.role == "worker"));
    assert!(
        entries
            .iter()
            .all(|e| e.provider.as_deref() == Some("12th")
                && e.model.as_deref() == Some("12th/glm-5.3")),
        "provider and model columns round-trip"
    );
    assert!(
        entries
            .iter()
            .any(|e| e.role == "planner" && e.iteration == Some(1))
    );
}

#[tokio::test]
async fn hysec_token_ledger_resumes_after_reconnect() {
    let path = temp_db();
    let session: SessionId = "hysec_ABCDEFGHIJKLMNOPQRST".parse().unwrap();
    {
        let store = SessionStore::connect(&path).await.unwrap();
        store
            .record_usage(&LedgerEntry {
                session,
                role: "worker".to_string(),
                iteration: Some(1),
                completion_run_id: Some("run-1".to_string()),
                prompt_tokens: 100,
                completion_tokens: 50,
                confidence: "actual".to_string(),
                provider: Some("12th".to_string()),
                model: Some("12th/glm-5.3".to_string()),
            })
            .await
            .unwrap();
    }

    let reopened = SessionStore::connect(&path).await.unwrap();
    let entries = reopened.read_usage(session).await.unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session, session);
    assert_eq!(entries[0].role, "worker");
    assert_eq!(entries[0].completion_run_id.as_deref(), Some("run-1"));
}

#[tokio::test]
async fn legacy_uuid_session_resumes_after_reconnect() {
    let path = temp_db();
    let session: SessionId = "ses_018f032a3d2f7a21a05c2e61fc57dced".parse().unwrap();
    {
        let store = SessionStore::connect(&path).await.unwrap();
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
    }

    let reopened = SessionStore::connect(&path).await.unwrap();
    let envelopes = reopened.replay(session).await.unwrap();
    let sessions = reopened.list_sessions().await.unwrap();

    assert_eq!(envelopes.len(), 1);
    assert_eq!(
        sessions
            .iter()
            .filter(|info| info.session == session)
            .count(),
        1
    );
}

#[tokio::test]
async fn saved_permission_resumes_after_reconnect() {
    let path = temp_db();
    let entry = SavedPermission {
        id: "psv_per_1".to_string(),
        project_id: "global".to_string(),
        action: "bash".to_string(),
        resource: "*".to_string(),
        time_created_ms: Some(1_700_000_000_000),
    };
    {
        let store = SessionStore::connect(&path).await.unwrap();
        store.save_permission(&entry).await.unwrap();
    }

    let reopened = SessionStore::connect(&path).await.unwrap();
    let saved = reopened.list_saved_permissions(None).await.unwrap();
    assert_eq!(saved, vec![entry.clone()]);

    assert_eq!(
        reopened.saved_permission("psv_per_1").await.unwrap(),
        Some(entry.clone())
    );

    reopened.remove_saved_permission("psv_per_1").await.unwrap();
    let reopened = SessionStore::connect(&path).await.unwrap();
    assert_eq!(reopened.list_saved_permissions(None).await.unwrap(), vec![]);
}

#[tokio::test]
async fn saved_permission_without_time_is_stamped_on_insert() {
    let store = SessionStore::connect_memory().await.unwrap();
    let entry = SavedPermission {
        id: "psv_per_2".to_string(),
        project_id: "global".to_string(),
        action: "tool".to_string(),
        resource: "write".to_string(),
        time_created_ms: None,
    };
    store.save_permission(&entry).await.unwrap();
    let saved = store.list_saved_permissions(None).await.unwrap();
    assert_eq!(saved.len(), 1);
    assert!(
        saved[0]
            .time_created_ms
            .is_some_and(|ms| ms > 1_600_000_000_000),
        "insert must stamp the creation time: {saved:?}"
    );
}
