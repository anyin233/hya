use sqlx::Row;

use crate::{SessionStore, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedPermission {
    pub id: String,
    pub project_id: String,
    pub action: String,
    pub resource: String,
}

impl SessionStore {
    pub async fn save_permission(&self, entry: &SavedPermission) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT OR IGNORE INTO saved_permission (id, project_id, action, resource) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&entry.id)
        .bind(&entry.project_id)
        .bind(&entry.action)
        .bind(&entry.resource)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_saved_permissions(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<SavedPermission>, StoreError> {
        let rows = match project_id {
            Some(project_id) => {
                sqlx::query(
                    "SELECT id, project_id, action, resource FROM saved_permission \
                     WHERE project_id = ? ORDER BY id",
                )
                .bind(project_id)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, project_id, action, resource FROM saved_permission ORDER BY id",
                )
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.into_iter().map(saved_permission).collect()
    }

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
    })
}
