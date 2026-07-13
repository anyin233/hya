use std::collections::BTreeMap;
use std::sync::Arc;

// allow: SIZE_OK - pending permission state, HTTP views, and SSE payloads share one owner.
use hya_proto::{Envelope, Event, MessageId, SessionId, ToolCallId};
use hya_store::{SessionStore, StoreError};
use hya_tool::{Action, AskRequest, Decision, Resource};
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};

use super::saved_permission::{SavedPermissions, action_name};

#[derive(Clone)]
pub(crate) struct PermissionRequests {
    inner: Arc<Mutex<BTreeMap<String, PendingPermission>>>,
    saved: SavedPermissions,
    events: broadcast::Sender<Value>,
}

struct PendingPermission {
    session: Option<SessionId>,
    message_id: Option<MessageId>,
    call_id: Option<ToolCallId>,
    action: Action,
    resource: Resource,
    reply: oneshot::Sender<Decision>,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct PermissionRequestView {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    action: String,
    resources: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    save: Option<Vec<String>>,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct LegacyPermissionRequestView {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    permission: String,
    patterns: Vec<String>,
    metadata: Value,
    always: Vec<String>,
    tool: PermissionToolView,
}

#[derive(Clone, serde::Serialize)]
struct PermissionToolView {
    #[serde(rename = "messageID")]
    message_id: String,
    #[serde(rename = "callID")]
    call_id: String,
}

#[derive(Clone, Copy)]
pub(crate) enum PermissionReply {
    Once,
    Always,
    Reject,
}

impl PermissionRequests {
    #[must_use]
    pub(crate) fn new(store: SessionStore) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            inner: Arc::default(),
            saved: SavedPermissions::new(store),
            events,
        }
    }

    #[must_use]
    pub(crate) fn spawn(mut rx: mpsc::UnboundedReceiver<AskRequest>, store: SessionStore) -> Self {
        let requests = Self::new(store.clone());
        let inner = requests.inner.clone();
        let events = requests.events.clone();
        std::mem::drop(tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let (message_id, call_id) = tool_correlation(&store, &req).await;
                let entry = PendingPermission {
                    session: req.session,
                    message_id,
                    call_id,
                    action: req.action,
                    resource: req.resource,
                    reply: req.reply,
                };
                let request_id = req.id.to_string();
                let asked = permission_asked_event(&request_id, &entry);
                inner.lock().await.insert(request_id, entry);
                let _published = events.send(asked);
            }
        }));
        requests
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    fn publish_replied(&self, session: Option<SessionId>, id: &str, reply: PermissionReply) {
        let _published = self
            .events
            .send(permission_replied_event(session, id, reply));
    }

    pub(crate) async fn list(&self) -> Vec<PermissionRequestView> {
        self.inner
            .lock()
            .await
            .iter()
            .filter_map(|(id, entry)| permission_view(id, entry))
            .collect()
    }

    pub(crate) async fn list_legacy(&self) -> Vec<LegacyPermissionRequestView> {
        self.inner
            .lock()
            .await
            .iter()
            .filter_map(|(id, entry)| {
                entry
                    .session
                    .map(|session| legacy_permission_view(id, entry, session.to_string()))
            })
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
    ) -> Result<bool, StoreError> {
        let (entry, related) = {
            let mut pending = self.inner.lock().await;
            let Some(entry) = pending.get(id) else {
                return Ok(false);
            };
            if entry.session != Some(session) {
                return Ok(false);
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
            return Ok(false);
        };
        let save_action = entry.action;
        let ok = entry.reply.send(decision(reply, message)).is_ok();
        if ok && matches!(reply, PermissionReply::Always) {
            self.saved.remember(id, save_action).await?;
        }
        for item in related {
            let _sent = item.reply.send(related_decision(reply));
        }
        if ok {
            self.publish_replied(Some(session), id, reply);
        }
        Ok(ok)
    }

    pub(crate) async fn reply_any(
        &self,
        id: &str,
        reply: PermissionReply,
        message: Option<String>,
    ) -> Result<bool, StoreError> {
        let (entry, related) = {
            let mut pending = self.inner.lock().await;
            let Some(entry) = pending.get(id) else {
                return Ok(false);
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
            return Ok(false);
        };
        let save_action = entry.action;
        let replied_session = entry.session;
        let ok = entry.reply.send(decision(reply, message)).is_ok();
        if ok && matches!(reply, PermissionReply::Always) {
            self.saved.remember(id, save_action).await?;
        }
        for item in related {
            let _sent = item.reply.send(related_decision(reply));
        }
        if ok {
            self.publish_replied(replied_session, id, reply);
        }
        Ok(ok)
    }

    pub(crate) async fn list_saved(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<super::SavedPermissionInfo>, StoreError> {
        self.saved.list(project_id).await
    }

    pub(crate) async fn remove_saved(&self, id: &str) -> Result<(), StoreError> {
        self.saved.remove(id).await
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

fn legacy_permission_view(
    id: &str,
    entry: &PendingPermission,
    session_id: String,
) -> LegacyPermissionRequestView {
    LegacyPermissionRequestView {
        id: id.to_string(),
        session_id,
        permission: action_name(entry.action),
        patterns: vec![entry.resource.pattern()],
        metadata: json!({}),
        always: vec!["*".to_string()],
        tool: PermissionToolView {
            message_id: entry
                .message_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            call_id: entry
                .call_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
        },
    }
}

async fn tool_correlation(
    store: &SessionStore,
    req: &AskRequest,
) -> (Option<MessageId>, Option<ToolCallId>) {
    if req.message_id.is_some() && req.call_id.is_some() {
        return (req.message_id, req.call_id);
    }
    let Some(session) = req.session else {
        return (req.message_id, req.call_id);
    };
    match store.replay(session).await {
        Ok(events) => fill_tool_correlation(req, &events),
        Err(_) => (req.message_id, req.call_id),
    }
}

fn fill_tool_correlation(
    req: &AskRequest,
    events: &[Envelope],
) -> (Option<MessageId>, Option<ToolCallId>) {
    let resource = req.resource.pattern();
    let matched = newest_tool_request(req, events, |input| {
        input_mentions_resource(input, &resource)
    });
    let fallback = matched.or_else(|| newest_tool_request(req, events, |_| true));
    (
        req.message_id
            .or_else(|| fallback.map(|(message, _)| message)),
        req.call_id.or_else(|| fallback.map(|(_, call)| call)),
    )
}

fn matches_correlation_filter(req: &AskRequest, message: &MessageId, call: &ToolCallId) -> bool {
    req.message_id.is_none_or(|id| id == *message) && req.call_id.is_none_or(|id| id == *call)
}

fn newest_tool_request<F>(
    req: &AskRequest,
    events: &[Envelope],
    accepts_input: F,
) -> Option<(MessageId, ToolCallId)>
where
    F: Fn(&Value) -> bool,
{
    events.iter().rev().find_map(|env| match &env.event {
        Event::ToolCallRequested {
            message,
            call,
            input,
            ..
        } if matches_correlation_filter(req, message, call) && accepts_input(input) => {
            Some((*message, *call))
        }
        _ => None,
    })
}

fn input_mentions_resource(input: &Value, resource: &str) -> bool {
    match input {
        Value::String(value) => {
            value == resource
                || resource.ends_with(&format!("/{value}"))
                || value.contains(resource)
        }
        Value::Array(values) => values
            .iter()
            .any(|value| input_mentions_resource(value, resource)),
        Value::Object(values) => values
            .values()
            .any(|value| input_mentions_resource(value, resource)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn permission_asked_event(request_id: &str, entry: &PendingPermission) -> Value {
    json!({
        "id": format!("evt_hya_perm_{request_id}"),
        "type": "permission.asked",
        "properties": legacy_permission_view(
            request_id,
            entry,
            entry.session.map(|session| session.to_string()).unwrap_or_default(),
        ),
    })
}

fn permission_replied_event(session: Option<SessionId>, id: &str, reply: PermissionReply) -> Value {
    json!({
        "id": format!("evt_hya_perm_reply_{id}"),
        "type": "permission.replied",
        "properties": {
            "sessionID": session.map(|session| session.to_string()).unwrap_or_default(),
            "requestID": id,
            "reply": reply_name(reply),
        },
    })
}

fn reply_name(reply: PermissionReply) -> &'static str {
    match reply {
        PermissionReply::Once => "once",
        PermissionReply::Always => "always",
        PermissionReply::Reject => "reject",
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use hya_proto::PermissionRequestId;

    #[test]
    fn permission_asked_event_includes_tool_correlation_when_available() {
        let message = MessageId::new();
        let call = ToolCallId::new();
        let (reply, _rx) = tokio::sync::oneshot::channel();
        let request_id = PermissionRequestId::new().to_string();
        let entry = PendingPermission {
            session: Some(SessionId::new()),
            message_id: Some(message),
            call_id: Some(call),
            action: Action::Bash,
            resource: Resource::Command("pwd".to_string()),
            reply,
        };

        let event = permission_asked_event(&request_id, &entry);

        assert_eq!(
            event["properties"]["tool"]["messageID"],
            message.to_string()
        );
        assert_eq!(event["properties"]["tool"]["callID"], call.to_string());
    }
}
