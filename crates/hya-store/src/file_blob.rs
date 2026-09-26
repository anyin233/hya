//! Per-session content-addressed file blobs for session revert.
//!
//! The engine records a file's content before a tool changes it (and before a
//! revert overwrites it) under its sha256 hash; events carry only the hash.
//! Rows are scoped to the session and removed with it.

use hya_proto::SessionId;
use sqlx::Row;

use crate::{SessionStore, StoreError};

impl SessionStore {
    /// Store `content` under `hash` for `session`. Storing an existing hash
    /// again is a no-op (the caller computed the hash from the content).
    ///
    /// # Errors
    /// Returns the SQLite error when the insert fails.
    pub async fn put_file_blob(
        &self,
        session: SessionId,
        hash: &str,
        content: &[u8],
    ) -> Result<(), StoreError> {
        let size = i64::try_from(content.len()).unwrap_or(i64::MAX);
        sqlx::query(
            "INSERT OR IGNORE INTO file_blob (session_id, hash, size, content) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(session.storage_key())
        .bind(hash)
        .bind(size)
        .bind(content)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The blob stored under `hash` for `session`, if any.
    ///
    /// # Errors
    /// Returns the SQLite error when the query fails.
    pub async fn file_blob(
        &self,
        session: SessionId,
        hash: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let row = sqlx::query("SELECT content FROM file_blob WHERE session_id = ? AND hash = ?")
            .bind(session.storage_key())
            .bind(hash)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|row| row.get::<Vec<u8>, _>(0)))
    }

    /// Total bytes of every blob stored for `session`.
    ///
    /// # Errors
    /// Returns the SQLite error when the query fails.
    pub async fn file_blob_bytes(&self, session: SessionId) -> Result<u64, StoreError> {
        let total: Option<i64> =
            sqlx::query_scalar("SELECT SUM(size) FROM file_blob WHERE session_id = ?")
                .bind(session.storage_key())
                .fetch_one(&self.pool)
                .await?;
        Ok(total.map_or(0, |total| u64::try_from(total).unwrap_or(0)))
    }
}
