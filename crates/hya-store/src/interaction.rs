//! Durable pending permission/question interaction metadata.
use hya_proto::SessionId;
use sqlx::Row;

/// A reply submitted while the owning process was handing off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingInteractionReply {
    /// Stable interaction identifier.
    pub id: String,
    /// Interaction plane (`permission` or `question`).
    pub kind: String,
    /// JSON reply submitted by the client.
    pub payload: String,
    /// Reply timestamp in Unix milliseconds.
    pub created_at: i64,
}

/// Durable metadata for one pending permission or question request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingInteraction {
    /// Stable client-visible request identifier.
    pub id: String,
    /// Session that owns the request, when correlated.
    pub session: Option<SessionId>,
    /// Interaction plane (`permission` or `question`).
    pub kind: String,
    /// JSON payload used to reconstruct the client request view.
    pub payload: String,
    /// Creation timestamp in Unix milliseconds.
    pub created_at: i64,
}

impl PendingInteraction {
    /// Construct a pending interaction with the current Unix-millisecond timestamp.
    pub fn new(
        id: impl Into<String>,
        session: Option<SessionId>,
        kind: impl Into<String>,
        payload: impl Into<String>,
    ) -> Self {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        Self {
            id: id.into(),
            session,
            kind: kind.into(),
            payload: payload.into(),
            created_at,
        }
    }
}

impl super::SessionStore {
    /// Insert or refresh unresolved interaction metadata.
    pub async fn save_pending_interaction(
        &self,
        row: &PendingInteraction,
    ) -> Result<(), super::StoreError> {
        sqlx::query("INSERT INTO pending_interaction (id, session_id, kind, payload, created_at, resolved_at) VALUES (?, ?, ?, ?, ?, NULL) ON CONFLICT(id) DO UPDATE SET session_id=excluded.session_id, kind=excluded.kind, payload=excluded.payload, resolved_at=NULL")
            .bind(&row.id).bind(row.session.map(|s| s.storage_key())).bind(&row.kind).bind(&row.payload).bind(row.created_at).execute(&self.pool).await?;
        Ok(())
    }

    /// List unresolved interaction metadata in stable creation order.
    pub async fn list_pending_interactions(
        &self,
    ) -> Result<Vec<PendingInteraction>, super::StoreError> {
        let rows = sqlx::query("SELECT id, session_id, kind, payload, created_at FROM pending_interaction WHERE resolved_at IS NULL ORDER BY created_at, id").fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let session = row
                    .try_get::<Option<Vec<u8>>, _>("session_id")?
                    .and_then(|v| String::from_utf8(v).ok()?.parse::<SessionId>().ok());
                Ok(PendingInteraction {
                    id: row.try_get("id")?,
                    session,
                    kind: row.try_get("kind")?,
                    payload: row.try_get("payload")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Mark one interaction resolved; false means another owner already resolved it.
    pub async fn resolve_pending_interaction(&self, id: &str) -> Result<bool, super::StoreError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let result = sqlx::query(
            "UPDATE pending_interaction SET resolved_at = ? WHERE id = ? AND resolved_at IS NULL",
        )
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Queue a reply durably when the in-memory oneshot owner is gone.
    pub async fn queue_pending_interaction_reply(
        &self,
        reply: &PendingInteractionReply,
    ) -> Result<(), super::StoreError> {
        sqlx::query("INSERT INTO pending_interaction_reply (id, kind, payload, created_at, consumed_at) VALUES (?, ?, ?, ?, NULL) ON CONFLICT(id) DO NOTHING")
            .bind(&reply.id).bind(&reply.kind).bind(&reply.payload).bind(reply.created_at).execute(&self.pool).await?;
        Ok(())
    }

    /// List replies that still need successor continuation.
    pub async fn list_pending_interaction_replies(
        &self,
    ) -> Result<Vec<PendingInteractionReply>, super::StoreError> {
        let rows = sqlx::query("SELECT id, kind, payload, created_at FROM pending_interaction_reply WHERE consumed_at IS NULL ORDER BY created_at, id").fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                Ok(PendingInteractionReply {
                    id: row.try_get("id")?,
                    kind: row.try_get("kind")?,
                    payload: row.try_get("payload")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Atomically claim a reply and resolve its interaction. This is the
    /// successor handoff fence: once claimed, a crash cannot cause the
    /// operator answer (or its continuation) to be applied a second time.
    pub async fn claim_pending_interaction_reply(
        &self,
        id: &str,
    ) -> Result<bool, super::StoreError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let reply = sqlx::query(
            "UPDATE pending_interaction_reply SET consumed_at = ? WHERE id = ? AND consumed_at IS NULL",
        )
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if reply.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "UPDATE pending_interaction SET resolved_at = ? WHERE id = ? AND resolved_at IS NULL",
        )
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}
