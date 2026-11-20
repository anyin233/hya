//! Per-session content-addressed blobs: file contents for session revert and
//! prompt image attachments.
//!
//! The engine records a file's content before a tool changes it (and before a
//! revert overwrites it) under its sha256 hash, and a prompt turn's images
//! under theirs; events carry only the hash. Rows are scoped to the session
//! and removed with it.

use hya_proto::{Envelope, Event, SessionId};
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
        if let Some(queue) = &self.write_queue {
            return queue
                .submit(
                    vec![(session, hash.to_string(), content.to_vec())],
                    Vec::new(),
                )
                .await
                .map(drop);
        }
        let size = i64::try_from(content.len()).unwrap_or(i64::MAX);
        sqlx::query(
            "INSERT OR IGNORE INTO file_blob (session_id, hash, size, content) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(session.storage_key())
        .bind(hash)
        .bind(size)
        .bind(content)
        .execute(&self.writer_pool)
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
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, StoreError> {
        if let Some(queue) = &self.write_queue {
            let blob_rows = blobs
                .iter()
                .map(|(hash, content)| (session, hash.clone(), content.clone()))
                .collect();
            let event_rows = events.into_iter().map(|ev| (session, ev)).collect();
            return queue.submit(blob_rows, event_rows).await;
        }
        let key = session.storage_key();
        let mut tx = self.begin_transaction().await?;
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
            envelopes.push(crate::append_event_in_transaction(&mut tx, session, event).await?);
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
        if let Some(queue) = &self.write_queue
            && let Some(content) = queue.queued_blob(session, hash)
        {
            return Ok(Some(content));
        }
        let pool = self.read_pool_checked()?;
        self.run_read_bounded("file_blob", async {
            let row =
                sqlx::query("SELECT content FROM file_blob WHERE session_id = ? AND hash = ?")
                    .bind(session.storage_key())
                    .bind(hash)
                    .fetch_optional(pool)
                    .await?;
            Ok(row.map(|row| row.get::<Vec<u8>, _>(0)))
        })
        .await
    }

    /// Total bytes of every blob stored for `session`.
    ///
    /// Queries SQLite (`SUM(size)`) plus acknowledged queued blobs whose hash is
    /// not yet on disk in one read transaction, bounded only while degraded
    /// (`run_read_bounded`). Fallback-guarded callers enforce `read_timeout()`
    /// themselves.
    ///
    /// # Errors
    /// Returns the SQLite error or `StorageTimeout` when the query fails or
    /// stalls past `write_timeout()` while degraded.
    pub async fn file_blob_bytes(&self, session: SessionId) -> Result<u64, StoreError> {
        let queued_sizes = self
            .write_queue
            .as_ref()
            .map(|queue| queue.queued_blob_sizes(session))
            .unwrap_or_default();
        let pool = self.read_pool_checked()?;
        self.run_read_bounded("file_blob_bytes", async {
            let key = session.storage_key();
            if queued_sizes.is_empty() {
                let total: i64 = sqlx::query_scalar(
                    "SELECT COALESCE(SUM(size), 0) FROM file_blob WHERE session_id = ?",
                )
                .bind(key)
                .fetch_one(pool)
                .await?;
                return Ok(u64::try_from(total).unwrap_or(0));
            }
            let mut conn = pool.acquire().await?;
            let mut tx = sqlx::Connection::begin(&mut *conn).await?;
            let disk_total: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(size), 0) FROM file_blob WHERE session_id = ?",
            )
            .bind(&key)
            .fetch_one(&mut *tx)
            .await?;
            let mut on_disk = std::collections::HashSet::with_capacity(queued_sizes.len());
            let hashes: Vec<&str> = queued_sizes.keys().map(String::as_str).collect();
            for chunk in hashes.chunks(500) {
                let mut qb =
                    sqlx::QueryBuilder::new("SELECT hash FROM file_blob WHERE session_id = ");
                qb.push_bind(&key);
                qb.push(" AND hash IN (");
                let mut sep = qb.separated(", ");
                for hash in chunk {
                    sep.push_bind(*hash);
                }
                sep.push_unseparated(")");
                let found: Vec<String> = qb.build_query_scalar().fetch_all(&mut *tx).await?;
                on_disk.extend(found);
            }
            tx.commit().await?;
            let queued_extra: u64 = queued_sizes
                .iter()
                .filter(|(hash, _)| !on_disk.contains(*hash))
                .map(|(_, size)| *size)
                .sum();
            Ok(u64::try_from(disk_total)
                .unwrap_or(0)
                .saturating_add(queued_extra))
        })
        .await
    }
}
