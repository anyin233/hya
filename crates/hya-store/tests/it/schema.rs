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
    assert!(matches!(error, StoreError::UnsupportedDatabase { .. }));
    let msg = error.to_string();
    assert!(
        msg.contains("hya-migration --db"),
        "expected migration guidance in error message: {msg}"
    );
    assert!(
        msg.contains(&db.0),
        "expected database path in error message: {msg}"
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
    assert!(matches!(error, StoreError::UnsupportedDatabase { .. }));
    assert!(error.to_string().contains("hya-migration --db"));
}

#[tokio::test]
async fn v1_resident_actor_claim_schema_is_rejected_with_migration_hint() {
    let db = TempDb::new();
    let pool = db.pool().await;
    sqlx::raw_sql(
        "CREATE TABLE _sqlx_migrations (
            version BIGINT PRIMARY KEY, description TEXT NOT NULL,
            installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL
        );
        CREATE TABLE event_log (seq INTEGER PRIMARY KEY, session_id BLOB, payload TEXT, ts INTEGER);
        CREATE TABLE resident_actor_claim (
            actor_id TEXT PRIMARY KEY,
            root_session_id BLOB NOT NULL,
            epoch INTEGER NOT NULL,
            owner_run_id TEXT NOT NULL,
            state TEXT NOT NULL
        );
        INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
        VALUES (1, 'init', 1, x'deadbeef', 0);",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let err = SessionStore::connect(&db.0)
        .await
        .err()
        .expect("v1 resident_actor_claim database must be rejected");
    assert!(matches!(err, StoreError::UnsupportedDatabase { .. }));
    let msg = err.to_string();
    assert!(msg.contains("hya-migration --db"), "msg: {msg}");
    assert!(msg.contains(&db.0), "msg: {msg}");

    let ro_err = SessionStore::connect_read_only(&db.0)
        .await
        .err()
        .expect("read-only open must also reject v1 database");
    assert!(matches!(ro_err, StoreError::UnsupportedDatabase { .. }));
    assert!(ro_err.to_string().contains("hya-migration --db"));
}

#[tokio::test]
async fn wrong_schema_meta_version_is_rejected_with_migration_hint() {
    let db = TempDb::new();
    drop(SessionStore::connect(&db.0).await.unwrap());
    let pool = db.pool().await;
    sqlx::query("UPDATE schema_meta SET value = '1' WHERE key = 'format_version'")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let err = SessionStore::connect(&db.0)
        .await
        .err()
        .expect("format_version = 1 must be rejected");
    assert!(matches!(err, StoreError::UnsupportedDatabase { .. }));
    assert!(err.to_string().contains("hya-migration --db"));
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
            "bundle_claim",
            "bundle_state",
            "event_log",
            "file_blob",
            "model_effort_preference",
            "open_assistant_message",
            "pending_resume",
            "project",
            "project_root",
            "projection_snapshot",
            "saved_permission",
            "schema_meta",
            "session"
        ]
    );
    let format_version: String =
        sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = 'format_version'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(format_version, "2");
    let versions: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(versions, vec![1]);
    let claim_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('bundle_claim')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        claim_columns,
        vec![
            "bundle",
            "claim_key",
            "owner",
            "epoch",
            "expires_at",
            "metadata",
            "acquired_at",
            "updated_at",
            "state"
        ]
    );
    let state_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('bundle_state')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        state_columns,
        vec!["bundle", "key", "value", "updated_at"]
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
