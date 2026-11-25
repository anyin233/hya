//! Durable model and reasoning effort preferences.

use hya_proto::{AgentName, OwnerRunId};
use sqlx::Row;

use crate::{SessionStore, StoreError, StoreTransaction};

const MAX_AGENT_ID_LENGTH: usize = 1_024;
const MAX_PROVIDER_ID_LENGTH: usize = 1_024;
const MAX_MODEL_ID_LENGTH: usize = 4_096;
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
        .fetch_all(self.read_pool_checked()?)
        .await?;
        rows.into_iter()
            .map(decode_agent_effort_preference)
            .collect()
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
        .fetch_optional(self.read_pool_checked()?)
        .await?;
        row.map(decode_agent_effort_preference).transpose()
    }

    /// Upsert a preference while holding the runtime owner fence.
    pub async fn upsert_agent_effort_preference(
        &self,
        owner: OwnerRunId,
        agent_id: &str,
        effort: &str,
        updated_at: i64,
    ) -> Result<(), StoreError> {
        validate_agent_effort_preference(agent_id, effort)?;
        let mut tx = self.begin_preference_write(owner).await?;
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
        validate_non_empty_trimmed(agent_id, "agent_id", MAX_AGENT_ID_LENGTH)?;
        let mut tx = self.begin_preference_write(owner).await?;
        sqlx::query("DELETE FROM agent_effort_preference WHERE agent_id = ?")
            .bind(agent_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// One durable base-model preference for a catalog Agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentModelPreference {
    /// Stable catalog Agent identity that owns this preference.
    pub agent: AgentName,
    /// Provider identity for the selected base model.
    pub provider_id: String,
    /// Provider-local identity for the selected base model.
    pub model_id: String,
}

impl SessionStore {
    /// List every stored Agent model preference in stable Agent-id order.
    ///
    /// The returned rows are decoded and validated before they leave the store,
    /// so a malformed auxiliary row is reported as typed store data rather than
    /// being silently exposed to callers.
    ///
    /// # Errors
    /// Returns a typed store-data error for malformed rows or a SQLite error
    /// when the query cannot be completed.
    pub async fn list_agent_model_preferences(
        &self,
    ) -> Result<Vec<AgentModelPreference>, StoreError> {
        let rows = sqlx::query(
            "SELECT agent_id, provider_id, model_id \
             FROM agent_model_preference ORDER BY agent_id",
        )
        .fetch_all(self.read_pool_checked()?)
        .await?;

        rows.into_iter()
            .map(decode_agent_model_preference)
            .collect()
    }

    /// Replace one Agent's durable model preference under the runtime-owner fence.
    ///
    /// The owner claim is checked before and after acquiring the SQLite writer
    /// transaction. The second check keeps the row replacement inside the same
    /// writer transaction that establishes the mutation boundary.
    ///
    /// # Errors
    /// Returns a typed store-data error when an identity is empty or exceeds its
    /// bound, [`StoreError::RuntimeOwnerClaimRequired`] without the matching
    /// owner claim, or a SQLite transaction error.
    pub async fn upsert_agent_model_preference(
        &self,
        owner: OwnerRunId,
        entry: &AgentModelPreference,
    ) -> Result<(), StoreError> {
        validate_agent_model_preference(entry)?;
        let mut tx = self.begin_preference_write(owner).await?;
        sqlx::query(
            "INSERT INTO agent_model_preference (agent_id, provider_id, model_id) \
             VALUES (?, ?, ?) \
             ON CONFLICT(agent_id) DO UPDATE SET \
                 provider_id = excluded.provider_id, \
                 model_id = excluded.model_id",
        )
        .bind(entry.agent.as_str())
        .bind(&entry.provider_id)
        .bind(&entry.model_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Remove one Agent's durable model preference under the runtime-owner fence.
    ///
    /// Removing an absent row succeeds, which makes clearing a preference
    /// idempotent while retaining the same owner and input validation rules as
    /// upsert.
    ///
    /// # Errors
    /// Returns a typed store-data error when the Agent identity is empty or
    /// exceeds its bound, [`StoreError::RuntimeOwnerClaimRequired`] without the
    /// matching owner claim, or a SQLite transaction error.
    pub async fn remove_agent_model_preference(
        &self,
        owner: OwnerRunId,
        agent: &AgentName,
    ) -> Result<(), StoreError> {
        validate_non_empty_trimmed(agent.as_str(), "agent_id", MAX_AGENT_ID_LENGTH)?;
        let mut tx = self.begin_preference_write(owner).await?;
        sqlx::query("DELETE FROM agent_model_preference WHERE agent_id = ?")
            .bind(agent.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// One persisted effort choice for an exact provider/model pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelEffortPreference {
    /// Provider route identifier.
    pub provider_id: String,
    /// Provider-local model identifier.
    pub model_id: String,
    /// Canonical effort label.
    pub effort: String,
    /// Unix timestamp in milliseconds of the last update.
    pub updated_at: i64,
}

impl SessionStore {
    /// List preferences in stable provider/model order.
    pub async fn list_model_effort_preferences(
        &self,
    ) -> Result<Vec<ModelEffortPreference>, StoreError> {
        let rows = sqlx::query(
            "SELECT provider_id, model_id, effort, updated_at
             FROM model_effort_preference ORDER BY provider_id, model_id",
        )
        .fetch_all(self.read_pool_checked()?)
        .await?;
        rows.into_iter()
            .map(decode_model_effort_preference)
            .collect()
    }

    /// Read one exact provider/model preference for live request resolution.
    pub async fn get_model_effort_preference(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Result<Option<ModelEffortPreference>, StoreError> {
        let row = sqlx::query(
            "SELECT provider_id, model_id, effort, updated_at
             FROM model_effort_preference WHERE provider_id = ? AND model_id = ?",
        )
        .bind(provider_id)
        .bind(model_id)
        .fetch_optional(self.read_pool_checked()?)
        .await?;
        row.map(decode_model_effort_preference).transpose()
    }

    /// Upsert a preference while holding the runtime owner fence.
    pub async fn upsert_model_effort_preference(
        &self,
        owner: OwnerRunId,
        provider_id: &str,
        model_id: &str,
        effort: &str,
        updated_at: i64,
    ) -> Result<(), StoreError> {
        validate_model_effort_preference(provider_id, model_id, effort)?;
        let mut tx = self.begin_preference_write(owner).await?;
        sqlx::query(
            "INSERT INTO model_effort_preference
                (provider_id, model_id, effort, updated_at) VALUES (?, ?, ?, ?)
             ON CONFLICT(provider_id, model_id) DO UPDATE SET
                effort = excluded.effort, updated_at = excluded.updated_at",
        )
        .bind(provider_id)
        .bind(model_id)
        .bind(effort)
        .bind(updated_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Clear a preference; absent rows are intentionally idempotent.
    pub async fn clear_model_effort_preference(
        &self,
        owner: OwnerRunId,
        provider_id: &str,
        model_id: &str,
    ) -> Result<(), StoreError> {
        validate_model_key(provider_id, model_id)?;
        let mut tx = self.begin_preference_write(owner).await?;
        sqlx::query("DELETE FROM model_effort_preference WHERE provider_id = ? AND model_id = ?")
            .bind(provider_id)
            .bind(model_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

impl SessionStore {
    async fn begin_preference_write(
        &self,
        owner: OwnerRunId,
    ) -> Result<StoreTransaction<'_>, StoreError> {
        self.require_runtime_owner(owner)?;
        let tx = self.begin_transaction().await?;
        self.require_runtime_owner(owner)?;
        Ok(tx)
    }
}

fn validate_agent_effort_preference(agent_id: &str, effort: &str) -> Result<(), StoreError> {
    validate_non_empty_trimmed(agent_id, "agent_id", MAX_AGENT_ID_LENGTH)?;
    validate_non_empty_trimmed(effort, "effort", MAX_EFFORT_LENGTH)
}

fn validate_agent_model_preference(entry: &AgentModelPreference) -> Result<(), StoreError> {
    validate_non_empty_trimmed(entry.agent.as_str(), "agent_id", MAX_AGENT_ID_LENGTH)?;
    validate_model_key(&entry.provider_id, &entry.model_id)
}

fn validate_model_effort_preference(
    provider_id: &str,
    model_id: &str,
    effort: &str,
) -> Result<(), StoreError> {
    validate_model_key(provider_id, model_id)?;
    validate_non_empty_trimmed(effort, "effort", MAX_EFFORT_LENGTH)
}

fn validate_model_key(provider_id: &str, model_id: &str) -> Result<(), StoreError> {
    validate_non_empty_trimmed(provider_id, "provider_id", MAX_PROVIDER_ID_LENGTH)?;
    validate_non_empty_trimmed(model_id, "model_id", MAX_MODEL_ID_LENGTH)
}

fn validate_non_empty_trimmed(
    value: &str,
    field: &'static str,
    maximum: usize,
) -> Result<(), StoreError> {
    if value.trim().is_empty() || value.chars().count() > maximum {
        return Err(StoreError::InvalidPreferenceData {
            field,
            detail: format!(
                "must contain a non-whitespace character and be at most {maximum} characters"
            ),
        });
    }
    Ok(())
}

fn decode_agent_effort_preference(
    row: sqlx::sqlite::SqliteRow,
) -> Result<AgentEffortPreference, StoreError> {
    let agent_id: String = row.try_get("agent_id")?;
    let effort: String = row.try_get("effort")?;
    validate_agent_effort_preference(&agent_id, &effort)?;
    Ok(AgentEffortPreference {
        agent_id,
        effort,
        updated_at: row.try_get("updated_at")?,
    })
}

fn decode_agent_model_preference(
    row: sqlx::sqlite::SqliteRow,
) -> Result<AgentModelPreference, StoreError> {
    let agent_id = decode_text(&row, "agent_id")?;
    let provider_id = decode_text(&row, "provider_id")?;
    let model_id = decode_text(&row, "model_id")?;
    let preference = AgentModelPreference {
        agent: AgentName::new(agent_id),
        provider_id,
        model_id,
    };
    validate_agent_model_preference(&preference)?;
    Ok(preference)
}

fn decode_text(row: &sqlx::sqlite::SqliteRow, field: &'static str) -> Result<String, StoreError> {
    row.try_get::<String, _>(field)
        .map_err(|error| StoreError::InvalidPreferenceData {
            field,
            detail: error.to_string(),
        })
}

fn decode_model_effort_preference(
    row: sqlx::sqlite::SqliteRow,
) -> Result<ModelEffortPreference, StoreError> {
    let provider_id: String = row.try_get("provider_id")?;
    let model_id: String = row.try_get("model_id")?;
    let effort: String = row.try_get("effort")?;
    validate_model_effort_preference(&provider_id, &model_id, &effort)?;
    Ok(ModelEffortPreference {
        provider_id,
        model_id,
        effort,
        updated_at: row.try_get("updated_at")?,
    })
}
