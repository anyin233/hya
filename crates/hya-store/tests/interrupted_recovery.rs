//! Crash recovery: a process that died with assistant turns open leaves them
//! closed (cause `interrupted`) by the next runtime owner, exactly once.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::{
    Event, FinishCause, FinishReason, MemberId, MemberRunStatus, MessageId, OwnerRunId, PartId,
    PartProjection, Role, SessionId, ToolCallId, ToolPartState,
};
use hya_store::SessionStore;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

fn temp_db() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "hya-interrupted-{nanos}-{}-{id}.db",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

fn remove_db(path: &str) {
    for suffix in ["", "-wal", "-shm", ".runtime-owner.lock"] {
        let _ = std::fs::remove_file(format!("{path}{suffix}"));
    }
}

/// One session whose assistant turn died mid-tool-call with a running member,
/// plus one session whose turn finished cleanly.
fn crashed_log(open: SessionId, done: SessionId) -> Vec<(SessionId, Event)> {
    let open_message = MessageId::new();
    let done_message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    vec![
        (
            open,
            Event::MessageStarted {
                session: open,
                message: open_message,
                role: Role::Assistant,
                agent: None,
                model: None,
            },
        ),
        (
            open,
            Event::ToolCallRequested {
                session: open,
                message: open_message,
                part,
                call,
                name: "bash".into(),
                input: serde_json::json!({"command": "sleep 100"}),
            },
        ),
        (
            open,
            Event::MemberSpawned {
                session: open,
                member: MemberId::new(),
                child: None,
                subagent_type: "general".into(),
                description: "worker".into(),
                depth: 1,
                directive: "work".into(),
                tool_call: None,
            },
        ),
        (
            done,
            Event::MessageStarted {
                session: done,
                message: done_message,
                role: Role::Assistant,
                agent: None,
                model: None,
            },
        ),
        (
            done,
            Event::MessageFinished {
                session: done,
                message: done_message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ),
    ]
}

fn finish_events(events: &[hya_proto::Envelope]) -> Vec<(FinishReason, Option<FinishCause>)> {
    events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::MessageFinished { finish, cause, .. } => Some((*finish, *cause)),
            _ => None,
        })
        .collect()
}

async fn assert_recovered(store: &SessionStore, open: SessionId, done: SessionId) {
    let events = store.replay(open).await.unwrap();
    assert_eq!(
        finish_events(&events),
        vec![(FinishReason::Cancelled, Some(FinishCause::Interrupted))],
        "the open turn is closed exactly once with cause interrupted"
    );
    let projection = store.read_projection(open).await.unwrap();
    let message = projection.session.messages.first().unwrap();
    assert_eq!(message.cause, Some(FinishCause::Interrupted));
    assert!(message.parts.iter().all(|part| !matches!(
        part,
        PartProjection::Tool {
            state: ToolPartState::Pending { .. } | ToolPartState::Running { .. },
            ..
        }
    )));
    assert!(
        projection
            .session
            .members
            .iter()
            .all(|member| member.status == MemberRunStatus::Cancelled)
    );
    let done_events = store.replay(done).await.unwrap();
    assert_eq!(
        finish_events(&done_events),
        vec![(FinishReason::Stop, None)],
        "a finished turn is never touched"
    );
}

#[tokio::test]
async fn recovery_closes_open_turns_once_and_is_idempotent_across_reopen() {
    let path = temp_db();
    let open = SessionId::new();
    let done = SessionId::new();
    {
        let store = SessionStore::connect(&path).await.unwrap();
        store.claim_runtime_owner(OwnerRunId::new()).unwrap();
        for (session, event) in crashed_log(open, done) {
            store.append_event(session, &event).await.unwrap();
        }
        // The process "dies" here: no drain, the store handle is dropped.
    }

    let store = SessionStore::connect(&path).await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let report = store.recover_interrupted_turns(owner).await.unwrap();
    assert_eq!(report.sessions, 1);
    assert_eq!(report.messages, 1);
    assert_recovered(&store, open, done).await;

    // Same owner again: nothing left to close.
    let again = store.recover_interrupted_turns(owner).await.unwrap();
    assert_eq!((again.sessions, again.messages, again.events), (0, 0, 0));
    drop(store);

    // A later process: still nothing to do, and the log is unchanged.
    let store = SessionStore::connect(&path).await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let later = store.recover_interrupted_turns(owner).await.unwrap();
    assert_eq!((later.sessions, later.messages, later.events), (0, 0, 0));
    assert_recovered(&store, open, done).await;
    drop(store);
    remove_db(&path);
}

#[tokio::test]
async fn recovery_requires_the_runtime_owner_claim() {
    let store = SessionStore::connect_memory().await.unwrap();
    assert!(
        store
            .recover_interrupted_turns(OwnerRunId::new())
            .await
            .is_err()
    );
}

/// A database written before the open-turn index existed: the migration
/// backfills the index from the event log, so the first recovery after the
/// upgrade still closes turns a crash left open.
#[tokio::test]
async fn upgrade_backfills_turns_left_open_before_the_index_existed() {
    let path = temp_db();
    let open = SessionId::new();
    let done = SessionId::new();
    {
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let mut migrator = sqlx::migrate!("./migrations");
        let before_index = migrator
            .migrations
            .iter()
            .filter(|migration| migration.version < 10)
            .cloned()
            .collect::<Vec<_>>();
        migrator.migrations = Cow::Owned(before_index);
        migrator.run(&pool).await.unwrap();
        for (session, event) in crashed_log(open, done) {
            sqlx::query("INSERT INTO event_log (session_id, payload, ts) VALUES (?, ?, 1)")
                .bind(session.storage_key())
                .bind(serde_json::to_string(&event).unwrap())
                .execute(&pool)
                .await
                .unwrap();
        }
        pool.close().await;
    }

    let store = SessionStore::connect(&path).await.unwrap();
    let owner = OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let report = store.recover_interrupted_turns(owner).await.unwrap();
    assert_eq!((report.sessions, report.messages), (1, 1));
    assert_recovered(&store, open, done).await;
    drop(store);
    remove_db(&path);
}
