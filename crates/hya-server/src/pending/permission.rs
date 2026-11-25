use std::collections::BTreeMap;
use std::sync::Arc;

// allow: SIZE_OK - pending permission state, HTTP views, and SSE payloads share one owner.
use hya_proto::{Envelope, Event, MessageId, PartProjection, SessionId, ToolCallId, ToolPartState};
use hya_store::{SessionStore, StoreError};
use hya_tool::{Action, AskRequest, Decision, RememberScope, Resource};
use serde_json::Value;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};

use super::PendingEvent;
use super::saved_permission::{SavedPermissions, action_name, install, uninstall};

#[derive(Clone)]
pub(crate) struct PermissionRequests {
    inner: Arc<Mutex<BTreeMap<String, PendingPermission>>>,
    saved: SavedPermissions,
    events: broadcast::Sender<PendingEvent>,
    store: SessionStore,
    #[cfg(test)]
    bridge_waiting: Arc<tokio::sync::Notify>,
}

struct PendingPermission {
    session: Option<SessionId>,
    message_id: Option<MessageId>,
    call_id: Option<ToolCallId>,
    /// Name and parsed arguments of the correlated tool call, read from the
    /// session projection when the ask arrived.
    tool: Option<ToolCallArgs>,
    action: Action,
    resource: Resource,
    remember: RememberScope,
    reply: oneshot::Sender<Decision>,
}

/// The tool call a permission ask is about, as recorded on its tool part.
#[derive(Clone)]
pub(crate) struct ToolCallArgs {
    pub(crate) name: String,
    pub(crate) input: Value,
}

#[derive(Clone)]
pub(crate) struct PermissionRequestView {
    pub(crate) id: String,
    pub(crate) session: Option<SessionId>,
    pub(crate) action: String,
    pub(crate) resource: String,
    pub(crate) always: Vec<String>,
    pub(crate) message_id: Option<MessageId>,
    pub(crate) call_id: Option<ToolCallId>,
    pub(crate) tool: Option<ToolCallArgs>,
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
            saved: SavedPermissions::new(store.clone()),
            events,
            store,
            #[cfg(test)]
            bridge_waiting: Arc::default(),
        }
    }

    #[must_use]
    pub(crate) fn spawn(
        mut rx: mpsc::UnboundedReceiver<AskRequest>,
        store: SessionStore,
        tasks: &crate::RuntimeTaskOwner,
    ) -> Self {
        let requests = Self::new(store.clone());
        let inner = requests.inner.clone();
        let events = requests.events.clone();
        #[cfg(test)]
        let bridge_waiting = requests.bridge_waiting.clone();
        crate::runtime_tasks::spawn_cancellable(&tasks.tasks, &tasks.stop, async move {
            while let Some(req) = rx.recv().await {
                let (message_id, call_id) = tool_correlation(&store, &req).await;
                let tool = match (req.session, call_id) {
                    (Some(session), Some(call)) => tool_call_args(&store, session, call).await,
                    _ => None,
                };
                let entry = PendingPermission {
                    session: req.session,
                    message_id,
                    call_id,
                    tool,
                    action: req.action,
                    resource: req.resource,
                    remember: req.remember,
                    reply: req.reply,
                };
                let request_id = req.id.to_string();
                // Decide under the pending lock: `allow_tree_once` runs after
                // the mode event is durable and takes the same lock, so an ask
                // racing a switch to `yolo` is either swept or allowed here.
                #[cfg(test)]
                bridge_waiting.notify_one();
                let mut pending = inner.lock().await;
                if let Some(session) = entry.session
                    && tree_is_yolo(&store, session).await
                {
                    drop(pending);
                    let _sent = entry.reply.send(Decision::AllowOnce);
                    continue;
                }
                let asked = PendingEvent::PermissionRequested(permission_view(&request_id, &entry));
                pending.insert(request_id, entry);
                drop(pending);
                let _published = events.send(asked);
            }
        });
        requests
    }

    /// Subscribe to live v1 interaction events.
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<PendingEvent> {
        self.events.subscribe()
    }

    #[cfg(test)]
    pub(crate) fn receiver_count(&self) -> usize {
        self.events.receiver_count()
    }

    fn publish_resolved(&self, session: Option<SessionId>, id: &str) {
        let _published = self.events.send(PendingEvent::Resolved {
            session,
            id: id.to_owned(),
        });
    }

    pub(crate) async fn list(&self) -> Vec<PermissionRequestView> {
        self.inner
            .lock()
            .await
            .iter()
            .filter(|(_, entry)| entry.session.is_some())
            .map(|(id, entry)| permission_view(id, entry))
            .collect()
    }

    pub(crate) async fn session_for(&self, id: &str) -> Option<SessionId> {
        self.inner.lock().await.get(id)?.session
    }

    pub(crate) async fn count(&self) -> usize {
        self.inner
            .lock()
            .await
            .values()
            .filter(|entry| entry.session.is_some())
            .count()
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
            let remember = entry.remember.clone();
            let entry = pending.remove(id);
            let related = take_related_for_reply(&mut pending, session, reply, &remember, action);
            (entry, related)
        };
        let Some(entry) = entry else {
            return Ok(false);
        };
        let save_action = entry.action;
        let save_remember = entry.remember.clone();
        let ok = entry.reply.send(decision(reply, message)).is_ok();
        if ok && matches!(reply, PermissionReply::Always) {
            self.saved.remember(id, save_action, &save_remember).await?;
        }
        for item in related {
            let _sent = item.reply.send(related_decision(reply));
        }
        if ok {
            self.publish_resolved(Some(session), id);
        }
        Ok(ok)
    }

    /// Allow once every pending ask whose session belongs to the tree rooted
    /// at `root` (its lineage root is `root`), publishing the usual replied
    /// notifications. Called after the tree switched to the `yolo` mode.
    pub(crate) async fn allow_tree_once(&self, root: SessionId) -> usize {
        let taken = {
            let mut pending = self.inner.lock().await;
            let mut ids = Vec::new();
            for (id, entry) in pending.iter() {
                if let Some(session) = entry.session
                    && lineage_root(&self.store, session).await == Some(root)
                {
                    ids.push(id.clone());
                }
            }
            ids.into_iter()
                .filter_map(|id| pending.remove(&id).map(|entry| (id, entry)))
                .collect::<Vec<_>>()
        };
        let mut allowed = 0;
        for (id, entry) in taken {
            if entry.reply.send(Decision::AllowOnce).is_ok() {
                allowed += 1;
                self.publish_resolved(entry.session, &id);
            }
        }
        allowed
    }

    pub(crate) async fn list_saved(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<hya_store::SavedPermission>, StoreError> {
        self.saved.list(project_id).await
    }

    /// Delete a saved grant and revoke it on `plane` (shared by every
    /// session plane), so the next matching call asks again.
    pub(crate) async fn remove_saved(
        &self,
        id: &str,
        plane: &hya_tool::PermissionPlane,
    ) -> Result<(), StoreError> {
        if let Some(row) = self.saved.remove(id).await? {
            uninstall(plane, &row).await;
        }
        Ok(())
    }

    /// Install every saved grant on `plane`; returns how many were restored.
    /// Global rows apply to every session; a Project's rows (ADR-0026) only
    /// to planes scoped to that Project.
    pub(crate) async fn restore_saved(
        &self,
        plane: &hya_tool::PermissionPlane,
    ) -> Result<usize, StoreError> {
        let mut restored = 0;
        for row in self.saved.list(None).await? {
            if install(plane, &row).await {
                restored += 1;
            }
        }
        Ok(restored)
    }
}

/// Lineage root of `session` (walks `parent` links), or `None` on a store
/// failure. Bounded like the engine's lineage walk.
async fn lineage_root(store: &SessionStore, session: SessionId) -> Option<SessionId> {
    let mut current = session;
    for _ in 0..1024 {
        match store
            .with_projection(current, |projection| projection.session.parent)
            .await
        {
            Ok(Some(parent)) => current = parent,
            Ok(None) => return Some(current),
            Err(_) => return None,
        }
    }
    Some(current)
}

/// Whether `session`'s tree records the `yolo` permission mode.
async fn tree_is_yolo(store: &SessionStore, session: SessionId) -> bool {
    let Some(root) = lineage_root(store, session).await else {
        return false;
    };
    store
        .with_projection(root, |projection| {
            projection.session.permission_mode.as_deref() == Some(hya_engine::permission_mode::YOLO)
        })
        .await
        .unwrap_or(false)
}

fn take_related_for_reply(
    pending: &mut BTreeMap<String, PendingPermission>,
    session: SessionId,
    reply: PermissionReply,
    remember: &RememberScope,
    action: Action,
) -> Vec<PendingPermission> {
    let scope = match (reply, remember) {
        (PermissionReply::Once, _) => return Vec::new(),
        (PermissionReply::Always, _) | (PermissionReply::Reject, RememberScope::Exact(_)) => {
            Some((remember, action))
        }
        (PermissionReply::Reject, RememberScope::LegacyAction | RememberScope::Scoped { .. }) => {
            None
        }
    };
    take_related(pending, session, scope)
}

fn take_related(
    pending: &mut BTreeMap<String, PendingPermission>,
    session: SessionId,
    scope: Option<(&RememberScope, Action)>,
) -> Vec<PendingPermission> {
    let ids: Vec<String> = pending
        .iter()
        .filter(|(_, entry)| {
            entry.session == Some(session)
                && scope.is_none_or(|(remember, action)| match remember {
                    RememberScope::LegacyAction => {
                        entry.action == action && entry.remember == RememberScope::LegacyAction
                    }
                    RememberScope::Exact(_) | RememberScope::Scoped { .. } => {
                        entry.remember == *remember
                    }
                })
        })
        .map(|(id, _)| id.clone())
        .collect();
    ids.into_iter()
        .filter_map(|id| pending.remove(&id))
        .collect()
}

fn permission_view(id: &str, entry: &PendingPermission) -> PermissionRequestView {
    PermissionRequestView {
        id: id.to_owned(),
        session: entry.session,
        action: action_name(entry.action),
        resource: entry.resource.pattern(),
        always: vec![entry.remember.pattern().to_owned()],
        message_id: entry.message_id,
        call_id: entry.call_id,
        tool: entry.tool.clone(),
    }
}

/// Name and arguments of tool call `call` from `session`'s projection: the
/// same input the transcript's tool part carries (no new data is exposed).
async fn tool_call_args(
    store: &SessionStore,
    session: SessionId,
    call: ToolCallId,
) -> Option<ToolCallArgs> {
    store
        .with_projection(session, |projection| {
            projection
                .session
                .messages
                .iter()
                .rev()
                .flat_map(|message| message.parts.iter())
                .find_map(|part| match part {
                    PartProjection::Tool {
                        call: part_call,
                        name,
                        state,
                        ..
                    } if *part_call == call => Some(ToolCallArgs {
                        name: name.to_string(),
                        input: tool_input(state).clone(),
                    }),
                    _ => None,
                })
        })
        .await
        .ok()
        .flatten()
}

fn tool_input(state: &ToolPartState) -> &Value {
    match state {
        ToolPartState::Pending { input }
        | ToolPartState::Running { input }
        | ToolPartState::Completed { input, .. }
        | ToolPartState::Error { input, .. } => input,
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
    use hya_tool::{ExactSubject, PermissionTarget, RememberScope};

    #[test]
    fn permission_view_includes_tool_correlation_when_available() {
        let message = MessageId::new();
        let call = ToolCallId::new();
        let (reply, _rx) = tokio::sync::oneshot::channel();
        let request_id = PermissionRequestId::new().to_string();
        let entry = PendingPermission {
            session: Some(SessionId::new()),
            message_id: Some(message),
            call_id: Some(call),
            tool: None,
            action: Action::Bash,
            resource: Resource::Command("pwd".to_string()),
            remember: RememberScope::LegacyAction,
            reply,
        };

        let view = permission_view(&request_id, &entry);
        assert_eq!(view.message_id, Some(message));
        assert_eq!(view.call_id, Some(call));
    }

    #[test]
    fn exact_always_groups_and_displays_only_the_identical_subject() {
        fn pending(session: SessionId, tool: &str, remember: RememberScope) -> PendingPermission {
            let (reply, _rx) = tokio::sync::oneshot::channel();
            PendingPermission {
                session: Some(session),
                message_id: None,
                call_id: None,
                tool: None,
                action: Action::Tool,
                resource: Resource::Tool(tool.to_string()),
                remember,
                reply,
            }
        }

        let session = SessionId::new();
        let write = RememberScope::Exact(ExactSubject::new(PermissionTarget::Tool, "write"));
        let edit = RememberScope::Exact(ExactSubject::new(PermissionTarget::Tool, "edit"));
        let mut requests = BTreeMap::from([
            ("same".to_string(), pending(session, "write", write.clone())),
            ("other".to_string(), pending(session, "edit", edit)),
            (
                "legacy".to_string(),
                pending(session, "write", RememberScope::LegacyAction),
            ),
        ]);

        let related = take_related(&mut requests, session, Some((&write, Action::Tool)));
        assert_eq!(related.len(), 1);
        assert!(requests.contains_key("other"));
        assert!(requests.contains_key("legacy"));

        let exact = pending(session, "write", write);
        assert_eq!(
            permission_view("id", &exact).always,
            vec!["write".to_string()]
        );
    }

    #[tokio::test]
    async fn exact_reject_groups_only_the_identical_subject()
    -> Result<(), Box<dyn std::error::Error>> {
        fn pending(
            session: SessionId,
            tool: &str,
            remember: RememberScope,
        ) -> (PendingPermission, oneshot::Receiver<Decision>) {
            let (reply, rx) = oneshot::channel();
            (
                PendingPermission {
                    session: Some(session),
                    message_id: None,
                    call_id: None,
                    tool: None,
                    action: Action::Tool,
                    resource: Resource::Tool(tool.to_string()),
                    remember,
                    reply,
                },
                rx,
            )
        }

        let store = SessionStore::connect_memory().await?;
        let requests = PermissionRequests::new(store);
        let session = SessionId::new();
        let write = RememberScope::Exact(ExactSubject::new(PermissionTarget::Tool, "write"));
        let edit = RememberScope::Exact(ExactSubject::new(PermissionTarget::Tool, "edit"));
        let (selected, selected_rx) = pending(session, "write", write.clone());
        let (same, same_rx) = pending(session, "write", write);
        let (other, _other_rx) = pending(session, "edit", edit);
        let (legacy, _legacy_rx) = pending(session, "write", RememberScope::LegacyAction);
        requests.inner.lock().await.extend([
            ("selected".to_string(), selected),
            ("same".to_string(), same),
            ("other".to_string(), other),
            ("legacy".to_string(), legacy),
        ]);

        assert!(
            requests
                .reply(session, "selected", PermissionReply::Reject, None)
                .await?
        );
        assert!(matches!(selected_rx.await?, Decision::Reject { .. }));
        assert!(matches!(same_rx.await?, Decision::Reject { .. }));
        let pending = requests.inner.lock().await;
        assert!(pending.contains_key("other"));
        assert!(pending.contains_key("legacy"));
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod shutdown_order_tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use hya_engine::{AgentSpec, CreateSession, EventBus, SessionEngine};
    use hya_proto::{AgentName, FinishCause, FinishReason, ModelRef, Role};
    use hya_provider::{Capabilities, CompletionRequest, EventStream, Provider, ProviderError};
    use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
    use hya_tool::{Mode, PermissionPlane, PermissionRules, Rule};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tower::ServiceExt;

    struct CountingProvider {
        inner: FakeProvider,
        rounds: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Provider for CountingProvider {
        fn id(&self) -> &str {
            self.inner.id()
        }
        fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
            self.inner.capabilities(model)
        }
        async fn stream(
            &self,
            request: CompletionRequest,
            session: SessionId,
            message: MessageId,
        ) -> Result<EventStream, ProviderError> {
            self.rounds.fetch_add(1, Ordering::SeqCst);
            self.inner.stream(request, session, message).await
        }
    }

    #[tokio::test]
    async fn restart_cancels_permission_wait_before_stopping_bridge() {
        for restart in [true, false] {
            let (permission, asks) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
                Action::Bash,
                "*",
                Mode::Ask,
            )]));
            let provider = FakeProvider::scripted_turns(vec![
                vec![
                    FakeStep::ToolCall {
                        name: "bash".into(),
                        input: serde_json::json!({"command":"true"}),
                    },
                    FakeStep::Finish(FinishReason::ToolCalls),
                ],
                vec![
                    FakeStep::Text("unexpected next provider round".into()),
                    FakeStep::Finish(FinishReason::Stop),
                ],
            ]);
            let provider = Arc::new(CountingProvider {
                inner: provider,
                rounds: AtomicUsize::new(0),
            });
            let engine = Arc::new(SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::new(ProviderRouter::new().with(provider.clone())),
                crate::test_support::test_runtime(Arc::new(hya_tool_loader::builtins())),
                permission,
                EventBus::default(),
            ));
            let agent = Arc::new(AgentSpec {
                name: AgentName::new("build"),
                model: ModelRef::new("fake"),
                system_prompt: String::new(),
                workdir: std::env::temp_dir(),
                reasoning: None,
            });
            let session = engine
                .create(CreateSession {
                    parent: None,
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                    agent: agent.name.clone(),
                    model: agent.model.clone(),
                    workdir: agent.workdir.to_string_lossy().into_owned(),
                })
                .await
                .unwrap();
            let app = crate::AppState::new(engine.clone(), agent).with_permission_requests(asks);
            let router = crate::router(app.clone());
            let st = app.server_state();
            // Hold the bridge at its pending-map await, with the ask reply still
            // owned by the bridge future rather than inserted into the map.
            let pending = st.permission_requests.inner.lock().await;
            let mut events = engine.bus().subscribe();
            let response = router
                .oneshot(
                    Request::post(format!("/v1/sessions/{session}/turns"))
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"prompt":{"text":"run"}}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(response.status().is_success());
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if matches!(
                        events.recv().await.unwrap().event,
                        Event::ToolCallRequested { .. }
                    ) {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            // All work is on this runtime thread: let the tool submit its ask and
            // the bridge reach the held lock before starting the stop.
            tokio::time::timeout(
                Duration::from_secs(5),
                st.permission_requests.bridge_waiting.notified(),
            )
            .await
            .unwrap();
            let tracked = st.tasks.token();
            let stopping = tokio::spawn(async move {
                if restart {
                    app.stop_for_restart(Duration::from_secs(5)).await.unwrap();
                } else {
                    app.shutdown().await;
                }
            });
            // Model another server service that joins only after the turn ends.
            // This exposes bridge failure before the old shutdown reaches drain.
            let finish = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let event = events.recv().await.unwrap().event;
                    if let Event::MessageFinished {
                        role: Role::Assistant,
                        finish,
                        cause,
                        ..
                    } = event
                    {
                        break (finish, cause);
                    }
                }
            })
            .await;
            drop(tracked);
            drop(pending);
            stopping.await.unwrap();
            assert_eq!(
                finish.unwrap(),
                (FinishReason::Cancelled, Some(FinishCause::Shutdown))
            );
            assert_eq!(
                provider.rounds.load(Ordering::SeqCst),
                1,
                "provider reopened during stop"
            );
            let log = engine.store().replay(session).await.unwrap();
            assert!(
                !log.iter()
                    .any(|e| matches!(&e.event, Event::ToolError { message_text, .. }
                if message_text.contains("unavailable")))
            );
            assert!(
                !log.iter()
                    .any(|e| matches!(&e.event, Event::TextDelta { delta, .. }
                if delta.contains("unexpected next provider round")))
            );
        }
    }
}
