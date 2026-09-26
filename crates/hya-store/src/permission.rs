//! Durable “allow always” permission rows used by the permission plane.

use sqlx::Row;

use crate::{SessionStore, StoreError};

/// One saved allow-always grant persisted for a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedPermission {
    /// Stable grant id (client- or host-assigned).
    pub id: String,
    /// Project scope key the grant applies under.
    pub project_id: String,
    /// Permission action string (e.g. tool / path class).
    pub action: String,
    /// Resource pattern or identifier the grant covers.
    pub resource: String,
    /// Creation time in milliseconds since the Unix epoch; `None` for rows
    /// saved before creation times were recorded. [`SessionStore::save_permission`]
    /// stamps the current time when this is `None`.
    pub time_created_ms: Option<i64>,
}

impl SessionStore {
    /// Insert a grant if the id is new (`INSERT OR IGNORE`); no-op on conflict.
    pub async fn save_permission(&self, entry: &SavedPermission) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT OR IGNORE INTO saved_permission \
             (id, project_id, action, resource, time_created) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&entry.id)
        .bind(&entry.project_id)
        .bind(&entry.action)
        .bind(&entry.resource)
        .bind(entry.time_created_ms.unwrap_or_else(now_ms))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// List grants, optionally filtered to one `project_id` (all projects when `None`).
    pub async fn list_saved_permissions(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<SavedPermission>, StoreError> {
        let rows =
            match project_id {
                Some(project_id) => sqlx::query(
                    "SELECT id, project_id, action, resource, time_created FROM saved_permission \
                     WHERE project_id = ? ORDER BY id",
                )
                .bind(project_id)
                .fetch_all(&self.pool)
                .await?,
                None => sqlx::query(
                    "SELECT id, project_id, action, resource, time_created FROM saved_permission \
                     ORDER BY id",
                )
                .fetch_all(&self.pool)
                .await?,
            };
        rows.into_iter().map(saved_permission).collect()
    }

    /// Read one grant by id.
    pub async fn saved_permission(&self, id: &str) -> Result<Option<SavedPermission>, StoreError> {
        let row = sqlx::query(
            "SELECT id, project_id, action, resource, time_created FROM saved_permission \
             WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(saved_permission).transpose()
    }

    /// Delete a grant by id (no error if the row is already absent).
    pub async fn remove_saved_permission(&self, id: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM saved_permission WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn saved_permission(row: sqlx::sqlite::SqliteRow) -> Result<SavedPermission, StoreError> {
    Ok(SavedPermission {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        action: row.try_get("action")?,
        resource: row.try_get("resource")?,
        time_created_ms: row.try_get("time_created")?,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}
