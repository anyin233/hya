//! Durable per-Agent model preference rows kept outside the Session event log.

use hya_proto::{AgentName, OwnerRunId};
use sqlx::Row;

use crate::{SessionStore, StoreError};

const MAX_AGENT_ID_LENGTH: usize = 1_024;
const MAX_PROVIDER_ID_LENGTH: usize = 1_024;
const MAX_MODEL_ID_LENGTH: usize = 4_096;

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
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(decode_preference).collect()
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
        validate_preference(entry)?;
        self.require_runtime_owner(owner)?;

        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        self.require_runtime_owner(owner)?;
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
        validate_agent(agent)?;
        self.require_runtime_owner(owner)?;

        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        self.require_runtime_owner(owner)?;
        sqlx::query("DELETE FROM agent_model_preference WHERE agent_id = ?")
            .bind(agent.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn validate_preference(entry: &AgentModelPreference) -> Result<(), StoreError> {
    validate_agent(&entry.agent)?;
    validate_text(&entry.provider_id, "provider_id", MAX_PROVIDER_ID_LENGTH)?;
    validate_text(&entry.model_id, "model_id", MAX_MODEL_ID_LENGTH)
}

fn validate_agent(agent: &AgentName) -> Result<(), StoreError> {
    validate_text(agent.as_str(), "agent_id", MAX_AGENT_ID_LENGTH)
}

fn validate_text(value: &str, field: &'static str, maximum: usize) -> Result<(), StoreError> {
    if value.is_empty() || value.chars().count() > maximum {
        return Err(StoreError::AdmissionData(format!(
            "agent model preference {field} must be non-empty and at most {maximum} characters",
        )));
    }
    Ok(())
}

fn decode_preference(row: sqlx::sqlite::SqliteRow) -> Result<AgentModelPreference, StoreError> {
    let agent_id = decode_text(&row, "agent_id")?;
    let provider_id = decode_text(&row, "provider_id")?;
    let model_id = decode_text(&row, "model_id")?;
    let preference = AgentModelPreference {
        agent: AgentName::new(agent_id),
        provider_id,
        model_id,
    };
    validate_preference(&preference)?;
    Ok(preference)
}

fn decode_text(row: &sqlx::sqlite::SqliteRow, field: &'static str) -> Result<String, StoreError> {
    row.try_get::<String, _>(field).map_err(|error| {
        StoreError::AdmissionData(format!(
            "agent model preference row has invalid {field}: {error}",
        ))
    })
}
