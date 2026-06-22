use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::{Mutex, mpsc, oneshot};
use yaca_proto::SessionId;
use yaca_tool::{Action, AskRequest, Decision, Resource};

#[derive(Clone, Default)]
pub(crate) struct PermissionRequests {
    inner: Arc<Mutex<BTreeMap<String, PendingPermission>>>,
    saved: Arc<Mutex<BTreeMap<String, SavedPermissionInfo>>>,
}

struct PendingPermission {
    session: Option<SessionId>,
    action: Action,
    resource: Resource,
    reply: oneshot::Sender<Decision>,
}

#[derive(Clone, Serialize)]
pub(crate) struct PermissionRequestView {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    action: String,
    resources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    save: Option<Vec<String>>,
}

#[derive(Clone, Copy)]
pub(crate) enum PermissionReply {
    Once,
    Always,
    Reject,
}

#[derive(Clone, Serialize)]
pub(crate) struct SavedPermissionInfo {
    id: String,
    #[serde(rename = "projectID")]
    project_id: String,
    action: String,
    resource: String,
}

impl PermissionRequests {
    #[must_use]
    pub(crate) fn spawn(mut rx: mpsc::UnboundedReceiver<AskRequest>) -> Self {
        let requests = Self::default();
        let inner = requests.inner.clone();
        std::mem::drop(tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let entry = PendingPermission {
                    session: req.session,
                    action: req.action,
                    resource: req.resource,
                    reply: req.reply,
                };
                inner.lock().await.insert(req.id.to_string(), entry);
            }
        }));
        requests
    }

    pub(crate) async fn list(&self) -> Vec<PermissionRequestView> {
        self.inner
            .lock()
            .await
            .iter()
            .filter_map(|(id, entry)| permission_view(id, entry))
            .collect()
    }

    pub(crate) async fn list_session(&self, session: SessionId) -> Vec<PermissionRequestView> {
        self.inner
            .lock()
            .await
            .iter()
            .filter_map(|(id, entry)| {
                (entry.session == Some(session))
                    .then(|| permission_view(id, entry))
                    .flatten()
            })
            .collect()
    }

    pub(crate) async fn reply(
        &self,
        session: SessionId,
        id: &str,
        reply: PermissionReply,
        message: Option<String>,
    ) -> bool {
        let (entry, related) = {
            let mut pending = self.inner.lock().await;
            let Some(entry) = pending.get(id) else {
                return false;
            };
            if entry.session != Some(session) {
                return false;
            }
            let action = entry.action;
            let entry = pending.remove(id);
            let related = match reply {
                PermissionReply::Once => Vec::new(),
                PermissionReply::Always => take_related(&mut pending, session, Some(action)),
                PermissionReply::Reject => take_related(&mut pending, session, None),
            };
            (entry, related)
        };
        let Some(entry) = entry else {
            return false;
        };
        let save_action = entry.action;
        let ok = entry.reply.send(decision(reply, message)).is_ok();
        if ok && matches!(reply, PermissionReply::Always) {
            self.remember_saved(id, save_action).await;
        }
        for item in related {
            let _sent = item.reply.send(related_decision(reply));
        }
        ok
    }

    pub(crate) async fn reply_any(
        &self,
        id: &str,
        reply: PermissionReply,
        message: Option<String>,
    ) -> bool {
        let (entry, related) = {
            let mut pending = self.inner.lock().await;
            let Some(entry) = pending.get(id) else {
                return false;
            };
            let action = entry.action;
            let session = entry.session;
            let entry = pending.remove(id);
            let related = match (reply, session) {
                (PermissionReply::Once, _) | (_, None) => Vec::new(),
                (PermissionReply::Always, Some(session)) => {
                    take_related(&mut pending, session, Some(action))
                }
                (PermissionReply::Reject, Some(session)) => {
                    take_related(&mut pending, session, None)
                }
            };
            (entry, related)
        };
        let Some(entry) = entry else {
            return false;
        };
        let save_action = entry.action;
        let ok = entry.reply.send(decision(reply, message)).is_ok();
        if ok && matches!(reply, PermissionReply::Always) {
            self.remember_saved(id, save_action).await;
        }
        for item in related {
            let _sent = item.reply.send(related_decision(reply));
        }
        ok
    }

    pub(crate) async fn list_saved(&self, project_id: Option<&str>) -> Vec<SavedPermissionInfo> {
        self.saved
            .lock()
            .await
            .values()
            .filter(|entry| project_id.is_none_or(|project_id| entry.project_id == project_id))
            .cloned()
            .collect()
    }

    pub(crate) async fn remove_saved(&self, id: &str) {
        self.saved.lock().await.remove(id);
    }

    async fn remember_saved(&self, request_id: &str, action: Action) {
        let action = action_name(action);
        let mut saved = self.saved.lock().await;
        if saved.values().any(|entry| {
            entry.project_id == "global" && entry.action == action && entry.resource == "*"
        }) {
            return;
        }
        let id = format!("psv_{request_id}");
        saved.insert(
            id.clone(),
            SavedPermissionInfo {
                id,
                project_id: "global".to_string(),
                action,
                resource: "*".to_string(),
            },
        );
    }
}

fn take_related(
    pending: &mut BTreeMap<String, PendingPermission>,
    session: SessionId,
    action: Option<Action>,
) -> Vec<PendingPermission> {
    let ids: Vec<String> = pending
        .iter()
        .filter(|(_, entry)| {
            entry.session == Some(session) && action.is_none_or(|action| entry.action == action)
        })
        .map(|(id, _)| id.clone())
        .collect();
    ids.into_iter()
        .filter_map(|id| pending.remove(&id))
        .collect()
}

fn permission_view(id: &str, entry: &PendingPermission) -> Option<PermissionRequestView> {
    Some(PermissionRequestView {
        id: id.to_string(),
        session_id: entry.session?.to_string(),
        action: action_name(entry.action),
        resources: vec![entry.resource.pattern()],
        save: Some(vec!["*".to_string()]),
    })
}

fn decision(reply: PermissionReply, message: Option<String>) -> Decision {
    match reply {
        PermissionReply::Once => Decision::AllowOnce,
        PermissionReply::Always => Decision::AllowAlways,
        PermissionReply::Reject => Decision::Reject { feedback: message },
    }
}

fn related_decision(reply: PermissionReply) -> Decision {
    match reply {
        PermissionReply::Once => Decision::AllowOnce,
        PermissionReply::Always => Decision::AllowAlways,
        PermissionReply::Reject => Decision::Reject { feedback: None },
    }
}

fn action_name(action: Action) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}
