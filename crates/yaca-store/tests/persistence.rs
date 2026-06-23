#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use yaca_proto::{Event, Projection, Role, SessionId};
use yaca_store::{LedgerEntry, SavedPermission, SessionStore};

fn temp_db() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!("yaca-persist-{nanos}-{}.db", std::process::id()))
        .to_string_lossy()
        .into_owned()
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
                },
            )
            .await
            .unwrap();
        let m = yaca_proto::MessageId::new();
        store
            .append_event(
                session,
                &Event::MessageStarted {
                    session,
                    message: m,
                    role: Role::Assistant,
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
            .any(|e| e.role == "planner" && e.iteration == Some(1))
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
    };
    {
        let store = SessionStore::connect(&path).await.unwrap();
        store.save_permission(&entry).await.unwrap();
    }

    let reopened = SessionStore::connect(&path).await.unwrap();
    let saved = reopened.list_saved_permissions(None).await.unwrap();
    assert_eq!(saved, vec![entry]);

    reopened.remove_saved_permission("psv_per_1").await.unwrap();
    let reopened = SessionStore::connect(&path).await.unwrap();
    assert_eq!(reopened.list_saved_permissions(None).await.unwrap(), vec![]);
}
