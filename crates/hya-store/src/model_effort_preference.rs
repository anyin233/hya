//! Durable per-model reasoning effort preferences.

use hya_proto::OwnerRunId;
use sqlx::Row;

use crate::{SessionStore, StoreError};

const MAX_PROVIDER_ID_LENGTH: usize = 1_024;
const MAX_MODEL_ID_LENGTH: usize = 4_096;
const MAX_EFFORT_LENGTH: usize = 64;

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
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(decode_preference).collect()
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
        .fetch_optional(&self.pool)
        .await?;
        row.map(decode_preference).transpose()
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
        validate(provider_id, model_id, effort)?;
        self.require_runtime_owner(owner)?;
        let mut tx = self.pool.begin().await?;
        self.require_runtime_owner(owner)?;
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
        validate(provider_id, model_id, "clear")?;
        self.require_runtime_owner(owner)?;
        let mut tx = self.pool.begin().await?;
        self.require_runtime_owner(owner)?;
        sqlx::query("DELETE FROM model_effort_preference WHERE provider_id = ? AND model_id = ?")
            .bind(provider_id)
            .bind(model_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn validate(provider: &str, model: &str, effort: &str) -> Result<(), StoreError> {
    for (value, field, max) in [
        (provider, "provider_id", MAX_PROVIDER_ID_LENGTH),
        (model, "model_id", MAX_MODEL_ID_LENGTH),
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

fn decode_preference(row: sqlx::sqlite::SqliteRow) -> Result<ModelEffortPreference, StoreError> {
    let provider_id: String = row.try_get("provider_id")?;
    let model_id: String = row.try_get("model_id")?;
    let effort: String = row.try_get("effort")?;
    validate(&provider_id, &model_id, &effort)?;
    Ok(ModelEffortPreference {
        provider_id,
        model_id,
        effort,
        updated_at: row.try_get("updated_at")?,
    })
}
