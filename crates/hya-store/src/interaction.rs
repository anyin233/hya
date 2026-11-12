//! Durable pending permission/question interaction metadata.
use hya_proto::SessionId;
use sqlx::Row;

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
                    .map(|key| crate::decode_session_key(&key))
                    .transpose()?;
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
}
