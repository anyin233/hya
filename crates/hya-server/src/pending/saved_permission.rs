use hya_store::{SavedPermission, SessionStore, StoreError};
use hya_tool::{Action, GrantScope, PermissionPlane, RememberScope};

/// `saved_permission.project_id` of rows that apply to every session: the
/// legacy action-wide and exact invocation grants.
pub(crate) const GLOBAL_PROJECT: &str = "global";

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

    /// Persist an "allow always" reply. Legacy and exact grants are stored
    /// under [`GLOBAL_PROJECT`]; a Project-scoped grant (ADR-0026) under its
    /// Project; a session-scoped grant is not persisted.
    pub(crate) async fn remember(
        &self,
        request_id: &str,
        action: Action,
        remember: &RememberScope,
    ) -> Result<(), StoreError> {
        let Some((project_id, resource)) = saved_row(remember) else {
            return Ok(());
        };
        let entry = SavedPermission {
            id: format!("psv_{request_id}"),
            project_id,
            action: action_name(action),
            resource,
            time_created_ms: None,
        };
        self.store.save_permission(&entry).await
    }
}

/// `(project_id, resource)` a remembered grant is saved under, or `None` when
/// it lasts only for its session (or its plane).
pub(crate) fn saved_row(remember: &RememberScope) -> Option<(String, String)> {
    match remember {
        RememberScope::Scoped {
            pattern,
            scope: Some(GrantScope::Project(project)),
        } => Some((project.clone(), pattern.clone())),
        RememberScope::Scoped { .. } => None,
        RememberScope::LegacyAction | RememberScope::Exact(_) => {
            Some((GLOBAL_PROJECT.to_string(), remember.pattern().to_string()))
        }
    }
}

/// Install one saved row on `plane`: global rows for every session, Project
/// rows only for that Project's sessions. Returns whether the action parsed.
pub(crate) async fn install(plane: &PermissionPlane, row: &SavedPermission) -> bool {
    let Some(action) = parse_action(&row.action) else {
        return false;
    };
    if row.project_id == GLOBAL_PROJECT {
        plane.grant_saved(action, &row.resource).await;
    } else {
        plane
            .grant_scoped(
                GrantScope::Project(row.project_id.clone()),
                action,
                &row.resource,
            )
            .await;
    }
    true
}

/// Revoke one saved row from `plane` (inverse of [`install`]).
pub(crate) async fn uninstall(plane: &PermissionPlane, row: &SavedPermission) {
    let Some(action) = parse_action(&row.action) else {
        return;
    };
    if row.project_id == GLOBAL_PROJECT {
        plane.revoke_saved(action, &row.resource).await;
    } else {
        plane
            .revoke_scoped(
                &GrantScope::Project(row.project_id.clone()),
                action,
                &row.resource,
            )
            .await;
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use hya_proto::SessionId;
    use hya_store::{SavedPermission, SessionStore};
    use hya_tool::{
        Action, AskRequest, GrantScope, PermissionPlane, PermissionRules, RememberScope, Resource,
    };
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::super::permission::{PermissionReply, PermissionRequests};
    use super::saved_row;

    const DIR: &str = "/outside/a/*";

    fn project(id: &str) -> GrantScope {
        GrantScope::Project(id.to_string())
    }

    /// Ask `ExternalDirectory` for [`DIR`] on `plane` through a spawned
    /// [`PermissionRequests`] bridge, reply `reply` to it, and return whether
    /// it asked at all.
    async fn ask_and_reply(
        requests: &PermissionRequests,
        plane: &PermissionPlane,
        session: SessionId,
        reply: PermissionReply,
    ) -> bool {
        ask_and_reply_for(requests, plane, session, DIR, reply).await
    }

    /// [`ask_and_reply`] for the resource `dir` instead of [`DIR`].
    async fn ask_and_reply_for(
        requests: &PermissionRequests,
        plane: &PermissionPlane,
        session: SessionId,
        dir: &'static str,
        reply: PermissionReply,
    ) -> bool {
        let mut events = requests.subscribe();
        let task = {
            let plane = plane.clone();
            tokio::spawn(async move {
                plane
                    .assert(Action::ExternalDirectory, Resource::Path(dir.to_string()))
                    .await
            })
        };
        let asked = tokio::time::timeout(Duration::from_millis(300), events.recv()).await;
        let Ok(Ok(event)) = asked else {
            task.await.unwrap().unwrap();
            return false;
        };
        let id = event["properties"]["id"].as_str().unwrap().to_string();
        assert!(requests.reply(session, &id, reply, None).await.unwrap());
        let _result = task.await.unwrap();
        true
    }

    fn bridge(store: &SessionStore) -> (PermissionPlane, PermissionRequests) {
        let (plane, rx): (PermissionPlane, UnboundedReceiver<AskRequest>) =
            PermissionPlane::new(PermissionRules::default());
        (plane, PermissionRequests::spawn(rx, store.clone()))
    }

    #[tokio::test]
    async fn project_allow_always_persists_the_concrete_directory_under_the_project() {
        let store = SessionStore::connect_memory().await.unwrap();
        let (plane, requests) = bridge(&store);
        let session = SessionId::new();
        let scoped = plane
            .for_session(session)
            .with_grant_scope(project("prj_a"));

        assert!(ask_and_reply(&requests, &scoped, session, PermissionReply::Always).await);

        let rows = store.list_saved_permissions(None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project_id, "prj_a");
        assert_eq!(rows[0].action, "externaldirectory");
        assert_eq!(rows[0].resource, DIR);
    }

    #[tokio::test]
    async fn session_scoped_allow_always_is_not_persisted() {
        let store = SessionStore::connect_memory().await.unwrap();
        let (plane, requests) = bridge(&store);
        let session = SessionId::new();
        let temporary = plane.for_session(session);

        assert!(ask_and_reply(&requests, &temporary, session, PermissionReply::Always).await);
        assert!(store.list_saved_permissions(None).await.unwrap().is_empty());
        assert!(
            !ask_and_reply(&requests, &temporary, session, PermissionReply::Once).await,
            "the session itself remembers the grant"
        );
        let other = SessionId::new();
        assert!(
            ask_and_reply(
                &requests,
                &plane.for_session(other),
                other,
                PermissionReply::Once
            )
            .await,
            "another session asks again"
        );
    }

    #[test]
    fn legacy_and_exact_grants_stay_global() {
        assert_eq!(
            saved_row(&RememberScope::LegacyAction),
            Some(("global".to_string(), "*".to_string()))
        );
        assert_eq!(
            saved_row(&RememberScope::Scoped {
                pattern: DIR.to_string(),
                scope: None,
            }),
            None
        );
    }

    #[tokio::test]
    async fn restore_installs_project_rows_for_that_project_only_and_remove_revokes() {
        let store = SessionStore::connect_memory().await.unwrap();
        for (id, project_id) in [("psv_a", "prj_a"), ("psv_global_web", "global")] {
            store
                .save_permission(&SavedPermission {
                    id: id.to_string(),
                    project_id: project_id.to_string(),
                    action: if project_id == "global" {
                        "webfetch"
                    } else {
                        "externaldirectory"
                    }
                    .to_string(),
                    resource: if project_id == "global" { "*" } else { DIR }.to_string(),
                    time_created_ms: None,
                })
                .await
                .unwrap();
        }
        let (plane, requests) = bridge(&store);
        assert_eq!(requests.restore_saved(&plane).await.unwrap(), 2);

        let a = SessionId::new();
        let b = SessionId::new();
        let in_a = plane.for_session(a).with_grant_scope(project("prj_a"));
        let in_b = plane.for_session(b).with_grant_scope(project("prj_b"));
        assert!(!ask_and_reply(&requests, &in_a, a, PermissionReply::Once).await);
        assert!(ask_and_reply(&requests, &in_b, b, PermissionReply::Once).await);
        in_b.assert(Action::WebFetch, Resource::Url("https://x".to_string()))
            .await
            .expect("global rows apply to every project");

        assert!(
            ask_and_reply_for(
                &requests,
                &in_a,
                a,
                "/outside/a/nested/*",
                PermissionReply::Once
            )
            .await,
            "a restored `<dir>/*` row grants exactly that directory, not a subdirectory"
        );
        assert!(
            ask_and_reply_for(&requests, &in_a, a, "/outside/*", PermissionReply::Once).await,
            "nor its parent"
        );

        requests.remove_saved("psv_a", &plane).await.unwrap();
        assert!(ask_and_reply(&requests, &in_a, a, PermissionReply::Once).await);
    }
}
