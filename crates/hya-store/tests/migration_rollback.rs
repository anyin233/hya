//! A restart that fails after the new build migrated the database rolls back
//! to the previous build, which must still open it: migrations applied by a
//! newer generation are tolerated (migrations stay additive).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_store::SessionStore;

#[tokio::test]
async fn a_database_migrated_by_a_newer_generation_still_opens() {
    let dir = std::env::temp_dir().join(format!("hya-migration-rollback-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("s.db");
    let path = path.to_str().unwrap().to_owned();

    let store = SessionStore::connect(&path).await.unwrap();
    drop(store);
    // What a newer build leaves behind: one more applied migration.
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{path}"))
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
         VALUES (9999, 'from a newer generation', 1, x'00', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    SessionStore::connect(&path)
        .await
        .expect("the previous build opens a database a newer build migrated");
    let _ = std::fs::remove_dir_all(dir);
}
