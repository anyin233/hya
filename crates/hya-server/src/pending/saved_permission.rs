use hya_store::{SavedPermission, SessionStore, StoreError};
use hya_tool::Action;

#[derive(Clone)]
pub(crate) struct SavedPermissions {
    store: SessionStore,
}

impl SavedPermissions {
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn new(store: SessionStore) -> Self {
        Self { store }
    }

    pub(crate) async fn list(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<SavedPermission>, StoreError> {
        self.store.list_saved_permissions(project_id).await
    }

    /// Delete a grant, returning the removed row (`None` when absent).
    pub(crate) async fn remove(&self, id: &str) -> Result<Option<SavedPermission>, StoreError> {
        let row = self.store.saved_permission(id).await?;
        if row.is_some() {
            self.store.remove_saved_permission(id).await?;
        }
        Ok(row)
    }

    #[allow(dead_code)]
    pub(crate) async fn remember(
        &self,
        request_id: &str,
        action: Action,
        resource: String,
    ) -> Result<(), StoreError> {
        // Grants are process-wide (the permission plane is shared by every
        // session and project), so rows are stored under the "global" scope.
        let entry = SavedPermission {
            id: format!("psv_{request_id}"),
            project_id: "global".to_string(),
            action: action_name(action),
            resource,
            time_created_ms: None,
        };
        self.store.save_permission(&entry).await
    }
}

pub(super) fn action_name(action: Action) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Parse a saved row's action name back into an [`Action`].
pub(crate) fn parse_action(name: &str) -> Option<Action> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}
