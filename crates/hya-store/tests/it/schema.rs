//! Session database schema boundary: initialize fresh stores, reject old formats.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_proto::{Event, SessionId};
use hya_store::{SessionStore, StoreError};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

struct TempDb(String);

impl TempDb {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("hya-schema-{}.db", SessionId::new()))
                .to_string_lossy()
                .into_owned(),
        )
    }

    async fn pool(&self) -> sqlx::SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&self.0)
                    .create_if_missing(true),
            )
            .await
            .unwrap()
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0));
        }
    }
}

#[tokio::test]
async fn old_schema_is_rejected_without_migrating() {
    let db = TempDb::new();
    let pool = db.pool().await;
    // Version 1 checksum from the pre-reset 0001_init.sql (the 19-migration schema).
    sqlx::raw_sql(
        "CREATE TABLE _sqlx_migrations (
            version BIGINT PRIMARY KEY, description TEXT NOT NULL,
            installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL
        );
        CREATE TABLE event_log (seq INTEGER PRIMARY KEY, session_id BLOB, payload TEXT, ts INTEGER);
        INSERT INTO event_log VALUES (1, x'01', 'old transcript', 123);
        CREATE TABLE message (id BLOB PRIMARY KEY);
        INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
        VALUES (1, 'init', 1, x'42367ed799566a115760304b216bba76a27c4a3b293d8a3a84445a7c634b7581716e9ad3c3cb1334e6c3433d2ba04185', 0),
               (19, 'agent effort preference', 1, x'00', 0);"
    ).execute(&pool).await.unwrap();
    pool.close().await;

    let error = SessionStore::connect(&db.0)
        .await
        .err()
        .expect("old database must fail at open");
    assert!(matches!(error, StoreError::UnsupportedDatabase));
    assert_eq!(
        error.to_string(),
        "this database was created by an older version and is no longer supported; move it away"
    );

    let pool = db.pool().await;
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(versions, vec![1, 19]);
    let payload: String = sqlx::query_scalar("SELECT payload FROM event_log WHERE seq = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(payload, "old transcript");
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(tables, vec!["_sqlx_migrations", "event_log", "message"]);
    pool.close().await;
}

#[tokio::test]
async fn unversioned_schema_is_rejected() {
    let db = TempDb::new();
    let pool = db.pool().await;
    sqlx::query("CREATE TABLE session (id BLOB PRIMARY KEY)")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let error = SessionStore::connect(&db.0)
        .await
        .err()
        .expect("unversioned schema must fail at open");
    assert!(matches!(error, StoreError::UnsupportedDatabase));
}

#[tokio::test]
async fn fresh_schema_reopens_and_contains_only_current_tables() {
    let db = TempDb::new();
    let store = SessionStore::connect(&db.0).await.unwrap();
    let session = SessionId::new();
    let event = Event::SessionTitled {
        session,
        title: "persisted".into(),
    };
    store.append_event(session, &event).await.unwrap();
    drop(store);
    let reopened = SessionStore::connect(&db.0).await.unwrap();
    assert_eq!(reopened.replay(session).await.unwrap()[0].event, event);
    drop(reopened);

    let pool = db.pool().await;
    let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name").fetch_all(&pool).await.unwrap();
    assert_eq!(
        tables,
        vec![
            "_sqlx_migrations",
            "agent_effort_preference",
            "agent_model_preference",
            "event_log",
            "file_blob",
            "model_effort_preference",
            "open_assistant_message",
            "pending_resume",
            "project",
            "project_root",
            "projection_snapshot",
            "resident_actor_claim",
            "saved_permission",
            "session"
        ]
    );
    let versions: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(versions, vec![1]);
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('resident_actor_claim')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        columns,
        vec![
            "actor_id",
            "root_session_id",
            "epoch",
            "owner_run_id",
            "state"
        ]
    );
    pool.close().await;
}

#[tokio::test]
async fn unknown_migration_is_rejected() {
    let db = TempDb::new();
    drop(SessionStore::connect(&db.0).await.unwrap());
    let pool = db.pool().await;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
         VALUES (9999, 'unknown schema', 1, x'00', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let error = SessionStore::connect(&db.0)
        .await
        .err()
        .expect("unknown migrations must not be ignored");
    assert!(matches!(error, StoreError::Migrate(_)));
}
