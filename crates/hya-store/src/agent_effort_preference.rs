//! Durable per-Agent default thinking effort, set by the user at runtime.

use hya_proto::OwnerRunId;
use sqlx::Row;

use crate::{SessionStore, StoreError};

const MAX_AGENT_ID_LENGTH: usize = 1_024;
const MAX_EFFORT_LENGTH: usize = 64;

/// One persisted effort choice for a stable Agent id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentEffortPreference {
    /// Stable catalog Agent id.
    pub agent_id: String,
    /// Canonical effort label.
    pub effort: String,
    /// Unix timestamp in milliseconds of the last update.
    pub updated_at: i64,
}

impl SessionStore {
    /// List preferences in stable Agent id order.
    pub async fn list_agent_effort_preferences(
        &self,
    ) -> Result<Vec<AgentEffortPreference>, StoreError> {
        let rows = sqlx::query(
            "SELECT agent_id, effort, updated_at FROM agent_effort_preference ORDER BY agent_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(decode_preference).collect()
    }

    /// Read one Agent's preference for live request resolution.
    pub async fn get_agent_effort_preference(
        &self,
        agent_id: &str,
    ) -> Result<Option<AgentEffortPreference>, StoreError> {
        let row = sqlx::query(
            "SELECT agent_id, effort, updated_at FROM agent_effort_preference WHERE agent_id = ?",
        )
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(decode_preference).transpose()
    }

    /// Upsert a preference while holding the runtime owner fence.
    pub async fn upsert_agent_effort_preference(
        &self,
        owner: OwnerRunId,
        agent_id: &str,
        effort: &str,
        updated_at: i64,
    ) -> Result<(), StoreError> {
        validate(agent_id, effort)?;
        self.require_runtime_owner(owner)?;
        let mut tx = self.pool.begin().await?;
        self.require_runtime_owner(owner)?;
        sqlx::query(
            "INSERT INTO agent_effort_preference (agent_id, effort, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(agent_id) DO UPDATE SET
                effort = excluded.effort, updated_at = excluded.updated_at",
        )
        .bind(agent_id)
        .bind(effort)
        .bind(updated_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Clear a preference; absent rows are intentionally idempotent.
    pub async fn clear_agent_effort_preference(
        &self,
        owner: OwnerRunId,
        agent_id: &str,
    ) -> Result<(), StoreError> {
        validate(agent_id, "clear")?;
        self.require_runtime_owner(owner)?;
        let mut tx = self.pool.begin().await?;
        self.require_runtime_owner(owner)?;
        sqlx::query("DELETE FROM agent_effort_preference WHERE agent_id = ?")
            .bind(agent_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn validate(agent_id: &str, effort: &str) -> Result<(), StoreError> {
    for (value, field, max) in [
        (agent_id, "agent_id", MAX_AGENT_ID_LENGTH),
        (effort, "effort", MAX_EFFORT_LENGTH),
    ] {
        if value.trim().is_empty() || value.chars().count() > max {
            return Err(StoreError::InvalidPreferenceData {
                field,
                detail: "must be non-empty and within the maximum length".to_string(),
            });
        }
    }
    Ok(())
}

fn decode_preference(row: sqlx::sqlite::SqliteRow) -> Result<AgentEffortPreference, StoreError> {
    let agent_id: String = row.try_get("agent_id")?;
    let effort: String = row.try_get("effort")?;
    validate(&agent_id, &effort)?;
    Ok(AgentEffortPreference {
        agent_id,
        effort,
        updated_at: row.try_get("updated_at")?,
    })
}
