//! Per-session content-addressed blobs: file contents for session revert and
//! prompt image attachments.
//!
//! The engine records a file's content before a tool changes it (and before a
//! revert overwrites it) under its sha256 hash, and a prompt turn's images
//! under theirs; events carry only the hash. Rows are scoped to the session
//! and removed with it.

use hya_proto::{Envelope, Event, EventSeq, SessionId, now_millis};
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

    /// Store `blobs` (`(hash, content)`) and append `events` for `session` in
    /// one transaction: either every blob and event is durable or none is.
    /// Returns the appended envelopes for the caller to publish.
    ///
    /// # Errors
    /// Returns the SQLite or serialization error; nothing is written then.
    pub async fn append_events_with_blobs(
        &self,
        session: SessionId,
        blobs: &[(String, Vec<u8>)],
        events: &[Event],
    ) -> Result<Vec<Envelope>, StoreError> {
        let key = session.storage_key();
        let mut tx = self.pool.begin().await?;
        for (hash, content) in blobs {
            let size = i64::try_from(content.len()).unwrap_or(i64::MAX);
            sqlx::query(
                "INSERT OR IGNORE INTO file_blob (session_id, hash, size, content) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&key)
            .bind(hash)
            .bind(size)
            .bind(content.as_slice())
            .execute(&mut *tx)
            .await?;
        }
        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            let ts_millis = now_millis();
            let payload = serde_json::to_string(event)?;
            let row = sqlx::query(
                "INSERT INTO event_log (session_id, payload, ts) VALUES (?, ?, ?) RETURNING seq",
            )
            .bind(&key)
            .bind(payload)
            .bind(ts_millis)
            .fetch_one(&mut *tx)
            .await?;
            let seq: i64 = row.try_get("seq")?;
            crate::materialize::materialize_event_side_tables(&mut tx, session, event, ts_millis)
                .await?;
            envelopes.push(Envelope {
                seq: EventSeq(seq.max(0) as u64),
                ts_millis,
                event: event.clone(),
            });
        }
        tx.commit().await?;
        Ok(envelopes)
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
