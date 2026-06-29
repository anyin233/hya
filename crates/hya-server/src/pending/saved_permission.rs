use hya_store::{SavedPermission, SessionStore, StoreError};
use hya_tool::Action;
use serde::Serialize;

#[derive(Clone)]
pub(crate) struct SavedPermissions {
    store: SessionStore,
}

#[derive(Clone, Serialize)]
pub(crate) struct SavedPermissionInfo {
    id: String,
    #[serde(rename = "projectID")]
    project_id: String,
    action: String,
    resource: String,
}

impl SavedPermissions {
    #[must_use]
    pub(crate) fn new(store: SessionStore) -> Self {
        Self { store }
    }

    pub(crate) async fn list(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<SavedPermissionInfo>, StoreError> {
        let saved = self.store.list_saved_permissions(project_id).await?;
        Ok(saved.into_iter().map(SavedPermissionInfo::from).collect())
    }

    pub(crate) async fn remove(&self, id: &str) -> Result<(), StoreError> {
        self.store.remove_saved_permission(id).await
    }

    pub(crate) async fn remember(
        &self,
        request_id: &str,
        action: Action,
    ) -> Result<(), StoreError> {
        let entry = SavedPermissionInfo {
            id: format!("psv_{request_id}"),
            project_id: "global".to_string(),
            action: action_name(action),
            resource: "*".to_string(),
        };
        self.store
            .save_permission(&SavedPermission::from(entry))
            .await
    }
}

impl From<SavedPermission> for SavedPermissionInfo {
    fn from(entry: SavedPermission) -> Self {
        Self {
            id: entry.id,
            project_id: entry.project_id,
            action: entry.action,
            resource: entry.resource,
        }
    }
}

impl From<SavedPermissionInfo> for SavedPermission {
    fn from(entry: SavedPermissionInfo) -> Self {
        Self {
            id: entry.id,
            project_id: entry.project_id,
            action: entry.action,
            resource: entry.resource,
        }
    }
}

pub(super) fn action_name(action: Action) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}
