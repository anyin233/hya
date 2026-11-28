#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_bundle::{
    ApiMethod, ApiScope, Bundle, BundleCatalog, ChannelCapability, ChannelParticipantRole,
    PreparedAgent,
};
use hya_proto::{
    ActorClaim, AgentName, ChannelKind, ChannelPolicySnapshot, Envelope, Event, EventSeq,
    MailEndpoint, MailKind, MessageId, ModelRef, Projection, SessionId, SubagentMode, ToolCallId,
    ToolName, ToolSchema,
};
use hya_runtime_abi::{
    BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
    BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionEventContext, ExtensionKind,
    HostBundleStorage, HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry,
    HostModelCaller, HostRuntime, HostSessionStore, HostTurnRunner, MemberMailRevivalRequest,
    ModelCallRequest, ModelCallResponse, ModelStreamChunk, ModelWindowInfo, NativeBundleApiHandler,
    NativeBundleApiRequest, NativeHook, NativeToolAfterInput, NativeToolAfterOutcome,
    NativeToolResult, ProjectionReducer, SessionListFilter, SessionSummaryMeta, StagedMailAck,
    TurnOutcome, TurnSpawnRequest, TurnStartRequest, TurnStatus, UriResolveCtx, UriSchemaHandler,
};
use hya_tool::extension::{BUNDLE_CHANNELS, BUNDLE_MULTI_AGENT, ExtensionEvent, team_projection};
use hya_tool::{
    AgentDef, InteractionPlane, LspPlane, MailboxPlane, PermissionPlane, PermissionRules,
    SkillCatalogEntry, SkillPlane, SpawnerPlane, Tool, ToolCtx, ToolError, handle::ArtifactPlane,
};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    AgentChannelsBundleApiHandler, AgentChannelsProjectionReducer, AgentChannelsService,
    ChannelPolicy, ChannelReadTool, ChannelUriSchemaHandler, ListChannelTool, SendTool,
    UnreadMailSteerHook, append_steer_notice, decode_channels_projection, parse_channel_uri,
};

struct MockHost {
    events: Mutex<HashMap<SessionId, Vec<Envelope>>>,
    parents: Mutex<HashMap<SessionId, Option<SessionId>>>,
    claims: Mutex<HashMap<(String, String), BundleClaimLease>>,
    policies: Mutex<HashMap<SessionId, ChannelPolicySnapshot>>,
    marks: Mutex<HashMap<SessionId, u64>>,
    notified: Mutex<Vec<(SessionId, u64, bool)>>,
    stopped_by_user: Mutex<Vec<(SessionId, String)>>,
    steer_suppressed: Mutex<HashMap<SessionId, bool>>,
    staged_acks: Mutex<HashMap<SessionId, StagedMailAck>>,
    revivals: Mutex<Vec<MemberMailRevivalRequest>>,
    bus_tx: broadcast::Sender<Envelope>,
    next_seq: Mutex<u64>,
}

impl MockHost {
    fn new() -> Arc<Self> {
        let (bus_tx, _) = broadcast::channel(64);
        Arc::new(Self {
            events: Mutex::new(HashMap::new()),
            parents: Mutex::new(HashMap::new()),
            claims: Mutex::new(HashMap::new()),
            policies: Mutex::new(HashMap::new()),
            marks: Mutex::new(HashMap::new()),
            notified: Mutex::new(Vec::new()),
            stopped_by_user: Mutex::new(Vec::new()),
            steer_suppressed: Mutex::new(HashMap::new()),
            staged_acks: Mutex::new(HashMap::new()),
            revivals: Mutex::new(Vec::new()),
            bus_tx,
            next_seq: Mutex::new(1),
        })
    }

    fn register_session(&self, session: SessionId, parent: Option<SessionId>) {
        self.parents.lock().unwrap().insert(session, parent);
        self.events.lock().unwrap().entry(session).or_default();
    }

    fn push_extension(&self, session: SessionId, event: ExtensionEvent) -> Envelope {
        let mut seq = self.next_seq.lock().unwrap();
        let current = *seq;
        *seq += 1;
        let env = Envelope {
            seq: EventSeq(current),
            ts_millis: 1_000,
            event: event.into_event(),
        };
        self.events
            .lock()
            .unwrap()
            .entry(session)
            .or_default()
            .push(env.clone());
        env
    }

    fn set_policy(&self, session: SessionId, policy: ChannelPolicySnapshot) {
        self.policies.lock().unwrap().insert(session, policy);
    }

    fn set_active_claim(&self, session: SessionId) {
        self.claims.lock().unwrap().insert(
            (BUNDLE_MULTI_AGENT.to_string(), session.to_string()),
            BundleClaimLease {
                bundle: BUNDLE_MULTI_AGENT.to_string(),
                resource_key: session.to_string(),
                owner_id: "owner-1".to_string(),
                epoch: 1,
                expires_at_millis: i64::MAX,
                metadata: Value::Null,
            },
        );
    }

    fn fold(&self, session: SessionId) -> Projection {
        let mut projection = Projection::default();
        let reducers = hya_tool::extension::projection_reducers();
        let log = self
            .events
            .lock()
            .unwrap()
            .get(&session)
            .cloned()
            .unwrap_or_default();
        for env in &log {
            projection.apply_with(env, &reducers);
        }
        projection
    }
}

fn allow_all_policy() -> ChannelPolicySnapshot {
    ChannelPolicySnapshot {
        unit_leader: 0b11111,
        unit_member: 0b11111,
        dm_parent: 0b11111,
        dm_child: 0b11111,
    }
}

#[async_trait]
impl HostSessionStore for MockHost {
    async fn session_exists(&self, session: SessionId) -> Result<bool, HostError> {
        Ok(self.parents.lock().unwrap().contains_key(&session))
    }

    async fn read_events(
        &self,
        session: SessionId,
        after_seq: Option<EventSeq>,
        limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError> {
        let mut out: Vec<Envelope> = self
            .events
            .lock()
            .unwrap()
            .get(&session)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| after_seq.is_none_or(|seq| e.seq.0 > seq.0))
            .collect();
        if let Some(limit) = limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    async fn append_events(
        &self,
        session: SessionId,
        _claim: Option<ActorClaim>,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, HostError> {
        let mut out = Vec::with_capacity(events.len());
        let mut seq = self.next_seq.lock().unwrap();
        let mut store = self.events.lock().unwrap();
        let log = store.entry(session).or_default();
        for event in events {
            let env = Envelope {
                seq: EventSeq(*seq),
                ts_millis: 1_000,
                event,
            };
            *seq += 1;
            log.push(env.clone());
            out.push(env);
        }
        Ok(out)
    }

    async fn append_extension_event(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
        claim: Option<ActorClaim>,
    ) -> Result<Envelope, HostError> {
        let mut envs = self
            .append_events(
                session,
                claim,
                vec![Event::Extension {
                    session,
                    bundle: bundle.to_string(),
                    kind: kind.to_string(),
                    payload,
                }],
            )
            .await?;
        Ok(envs.remove(0))
    }

    async fn read_projection(&self, session: SessionId) -> Result<Arc<Projection>, HostError> {
        Ok(Arc::new(self.fold(session)))
    }

    async fn read_extension_state(
        &self,
        session: SessionId,
        bundle: &str,
    ) -> Result<Option<Value>, HostError> {
        Ok(self.fold(session).extensions.get(bundle).cloned())
    }

    async fn session_metadata(
        &self,
        session: SessionId,
    ) -> Result<Option<SessionSummaryMeta>, HostError> {
        let parents = self.parents.lock().unwrap();
        let Some(&parent) = parents.get(&session) else {
            return Ok(None);
        };
        Ok(Some(SessionSummaryMeta {
            session,
            parent,
            agent: Some(AgentName::new("build")),
            model: None,
            workdir: Some(".".to_string()),
            title: None,
            project: None,
            archived: false,
            metadata: Value::Null,
            event_count: 0,
        }))
    }

    async fn list_sessions(
        &self,
        _filter: SessionListFilter,
    ) -> Result<Vec<SessionSummaryMeta>, HostError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl HostBundleStorage for MockHost {
    async fn get_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _key: &str,
    ) -> Result<Option<BundleStateEntry>, HostError> {
        Ok(None)
    }

    async fn put_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
        value: Value,
        _expected_version: Option<u64>,
    ) -> Result<BundleStateEntry, HostError> {
        Ok(BundleStateEntry {
            bundle: bundle.to_string(),
            scope: scope.to_string(),
            key: key.to_string(),
            value,
            version: 1,
            updated_at_millis: 0,
        })
    }

    async fn delete_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _key: &str,
    ) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn list_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _prefix: Option<&str>,
    ) -> Result<Vec<BundleStateEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn acquire_claim(
        &self,
        request: BundleClaimRequest,
    ) -> Result<BundleClaimLease, HostError> {
        let lease = BundleClaimLease {
            bundle: request.bundle.clone(),
            resource_key: request.resource_key.clone(),
            owner_id: request.owner_id,
            epoch: 1,
            expires_at_millis: i64::MAX,
            metadata: request.metadata,
        };
        self.claims
            .lock()
            .unwrap()
            .insert((request.bundle, request.resource_key), lease.clone());
        Ok(lease)
    }

    async fn renew_claim(
        &self,
        lease: &BundleClaimLease,
        _ttl_millis: u64,
    ) -> Result<BundleClaimLease, HostError> {
        Ok(lease.clone())
    }

    async fn release_claim(&self, lease: &BundleClaimLease) -> Result<(), HostError> {
        self.claims
            .lock()
            .unwrap()
            .remove(&(lease.bundle.clone(), lease.resource_key.clone()));
        Ok(())
    }

    async fn read_claim(
        &self,
        bundle: &str,
        resource_key: &str,
    ) -> Result<Option<BundleClaimLease>, HostError> {
        Ok(self
            .claims
            .lock()
            .unwrap()
            .get(&(bundle.to_string(), resource_key.to_string()))
            .cloned())
    }
}

#[async_trait]
impl HostTurnRunner for MockHost {
    async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn run_turn(
        &self,
        _request: TurnStartRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn spawn_session_and_run(
        &self,
        _request: TurnSpawnRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn steer_turn(&self, _session: SessionId, _message: String) -> Result<(), HostError> {
        Ok(())
    }

    async fn cancel_turn(
        &self,
        _session: SessionId,
        _reason: Option<String>,
    ) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn turn_status(&self, _session: SessionId) -> Result<TurnStatus, HostError> {
        Ok(TurnStatus::Idle)
    }

    async fn session_channel_policy(
        &self,
        session: SessionId,
    ) -> Result<Option<ChannelPolicySnapshot>, HostError> {
        Ok(self.policies.lock().unwrap().get(&session).copied())
    }

    fn mail_mark(&self, session: SessionId) -> Option<u64> {
        self.marks.lock().unwrap().get(&session).copied()
    }

    fn notify_mail_consumed(&self, session: SessionId, mark: u64, consumed_incoming: bool) {
        self.notified
            .lock()
            .unwrap()
            .push((session, mark, consumed_incoming));
    }

    fn is_member_stopped_by_user(&self, root: SessionId, handle: &str) -> bool {
        self.stopped_by_user
            .lock()
            .unwrap()
            .iter()
            .any(|(r, h)| *r == root && h == handle)
    }

    fn is_mail_steer_suppressed(&self, session: SessionId) -> bool {
        self.steer_suppressed
            .lock()
            .unwrap()
            .get(&session)
            .copied()
            .unwrap_or(false)
    }

    fn stage_mail_steer_ack(&self, session: SessionId, ack: StagedMailAck) {
        self.staged_acks.lock().unwrap().insert(session, ack);
    }

    async fn revive_archived_member_with_mail(
        &self,
        request: MemberMailRevivalRequest,
    ) -> Result<usize, HostError> {
        self.revivals.lock().unwrap().push(request);
        Ok(1)
    }
}

#[async_trait]
impl HostModelCaller for MockHost {
    async fn complete(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelCallResponse, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn stream(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn compact_window(
        &self,
        _model: &ModelRef,
        _messages: &[hya_proto::Message],
    ) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    async fn model_window(&self, _model: &ModelRef) -> Result<Option<ModelWindowInfo>, HostError> {
        Ok(None)
    }
}

#[async_trait]
impl HostEventBus for MockHost {
    fn publish(&self, envelope: Envelope) -> Result<(), HostError> {
        let _ = self.bus_tx.send(envelope);
        Ok(())
    }

    fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
        Ok(self.bus_tx.subscribe())
    }

    async fn publish_extension(
        &self,
        _session: SessionId,
        _bundle: &str,
        _kind: &str,
        _payload: Value,
    ) -> Result<(), HostError> {
        Ok(())
    }
}

#[async_trait]
impl HostCatalogReader for MockHost {
    async fn list_agents(&self, _session: Option<SessionId>) -> Result<Vec<AgentDef>, HostError> {
        Ok(Vec::new())
    }

    async fn get_agent(
        &self,
        _name: &AgentName,
        _session: Option<SessionId>,
    ) -> Result<Option<PreparedAgent>, HostError> {
        Ok(None)
    }

    async fn list_skills(
        &self,
        _session: Option<SessionId>,
    ) -> Result<Vec<SkillCatalogEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn list_bundles(&self) -> Result<Vec<BundleCatalogSummary>, HostError> {
        Ok(Vec::new())
    }

    async fn get_bundle(&self, _id: &str) -> Result<Option<Bundle>, HostError> {
        Ok(None)
    }

    async fn bundle_config(&self, _bundle_id: &str) -> Result<Value, HostError> {
        Ok(Value::Null)
    }

    async fn global_config(&self) -> Result<Value, HostError> {
        Ok(Value::Null)
    }
}

#[async_trait]
impl HostExtensionRegistry for MockHost {
    async fn list_extensions(
        &self,
        _kind: Option<ExtensionKind>,
    ) -> Result<Vec<ExtensionEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn has_bundle(&self, _bundle_id: &str) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn invoke_bundle_api(
        &self,
        _invocation: BundleApiInvocation,
    ) -> Result<BundleApiResult, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }

    async fn invoke_tool(
        &self,
        _session: Option<SessionId>,
        _tool_name: &str,
        _input: Value,
        _cancel: CancellationToken,
    ) -> Result<Value, HostError> {
        Err(HostError::Unavailable("mock".into()))
    }
}

impl HostRuntime for MockHost {
    fn session_store(&self) -> &dyn HostSessionStore {
        self
    }
    fn bundle_storage(&self) -> &dyn HostBundleStorage {
        self
    }
    fn turn_runner(&self) -> &dyn HostTurnRunner {
        self
    }
    fn model_caller(&self) -> &dyn HostModelCaller {
        self
    }
    fn event_bus(&self) -> &dyn HostEventBus {
        self
    }
    fn catalog_reader(&self) -> &dyn HostCatalogReader {
        self
    }
    fn extension_registry(&self) -> &dyn HostExtensionRegistry {
        self
    }
}

fn tool_ctx(session: SessionId, policy: ChannelPolicySnapshot) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![]));
    let (spawner, _srx) = SpawnerPlane::new();
    let (interaction, _irx) = InteractionPlane::new();
    ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission: permission.for_session(session),
        interaction: interaction.for_session(session),
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(ToolCallId::new()),
        mailbox: MailboxPlane::disconnected().with_channel_policy(policy),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(session),
        parent_session: None,
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir: PathBuf::from("."),
        roots: vec![PathBuf::from(".")],
        cancel: CancellationToken::new(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    }
}

fn setup_team() -> (Arc<MockHost>, SessionId, SessionId, String, String) {
    let host = MockHost::new();
    let root = SessionId::new();
    let child = SessionId::new();
    host.register_session(root, None);
    host.register_session(child, Some(root));
    host.set_policy(root, allow_all_policy());
    host.set_policy(child, allow_all_policy());
    host.set_active_claim(child);

    host.push_extension(
        root,
        ExtensionEvent::AgentRegistered {
            session: root,
            agent_session: root,
            handle: "main".to_string(),
            parent: None,
            agent_type: AgentName::new("build"),
            mode: SubagentMode::Transient,
        },
    );
    host.push_extension(
        root,
        ExtensionEvent::AgentRegistered {
            session: root,
            agent_session: child,
            handle: "scout-1".to_string(),
            parent: Some("main".to_string()),
            agent_type: AgentName::new("scout"),
            mode: SubagentMode::Resident,
        },
    );
    let group = "announce-12345678".to_string();
    let dm = "DM-87654321".to_string();
    host.push_extension(
        root,
        ExtensionEvent::ChannelCreated {
            session: root,
            channel: group.clone(),
            kind: ChannelKind::Group,
            unit: Some("main".to_string()),
            members: vec!["main".to_string(), "main/scout-1".to_string()],
        },
    );
    host.push_extension(
        root,
        ExtensionEvent::ChannelCreated {
            session: root,
            channel: dm.clone(),
            kind: ChannelKind::Dm,
            unit: None,
            members: vec!["main".to_string(), "main/scout-1".to_string()],
        },
    );
    (host, root, child, group, dm)
}

#[test]
fn projection_reducer_folds_channels_mail_and_cursors() {
    let reducer = AgentChannelsProjectionReducer;
    let session = SessionId::new();
    let mut state = reducer.initial_state();

    let created = ExtensionEvent::ChannelCreated {
        session,
        channel: "DM-11112222".to_string(),
        kind: ChannelKind::Dm,
        unit: None,
        members: vec!["main".to_string(), "main/w1".to_string()],
    }
    .into_event();
    let Event::Extension { kind, payload, .. } = created else {
        unreachable!()
    };
    reducer
        .apply(
            &mut state,
            &ExtensionEventContext {
                session,
                seq: EventSeq(1),
                ts_millis: 1,
                bundle: BUNDLE_CHANNELS.to_string(),
                kind,
                payload,
            },
        )
        .unwrap();

    let sent = ExtensionEvent::MailSent {
        session,
        from: "main/w1".to_string(),
        to: MailEndpoint::Channel("DM-11112222".to_string()),
        kind: MailKind::Message,
        body: "hello".to_string(),
    }
    .into_event();
    let Event::Extension { kind, payload, .. } = sent else {
        unreachable!()
    };
    reducer
        .apply(
            &mut state,
            &ExtensionEventContext {
                session,
                seq: EventSeq(2),
                ts_millis: 2,
                bundle: BUNDLE_CHANNELS.to_string(),
                kind,
                payload,
            },
        )
        .unwrap();

    let consumed = ExtensionEvent::MailConsumed {
        session,
        handle: "main".to_string(),
        through: 1,
    }
    .into_event();
    let Event::Extension { kind, payload, .. } = consumed else {
        unreachable!()
    };
    reducer
        .apply(
            &mut state,
            &ExtensionEventContext {
                session,
                seq: EventSeq(3),
                ts_millis: 3,
                bundle: BUNDLE_CHANNELS.to_string(),
                kind,
                payload,
            },
        )
        .unwrap();

    let mut projection = Projection::default();
    projection
        .extensions
        .insert(BUNDLE_CHANNELS.to_string(), state);
    let decoded = decode_channels_projection(&projection);
    assert_eq!(decoded.mail.len(), 1);
    assert_eq!(decoded.inboxes["main"], vec![0]);
    assert_eq!(decoded.cursors["main"], 1);
}

#[tokio::test]
async fn send_list_and_channel_uri_handler_round_trip() {
    let (host, root, child, group, dm) = setup_team();
    host.marks.lock().unwrap().insert(root, 7);
    let service = Arc::new(AgentChannelsService::with_runtime(
        Arc::clone(&host) as Arc<dyn HostRuntime>
    ));

    // Child sends DM to parent via default channel (`channel` omitted).
    let send = SendTool::new(Arc::clone(&service));
    let out = send
        .execute(
            &tool_ctx(child, allow_all_policy()),
            json!({ "body": "status ok" }),
        )
        .await
        .unwrap();
    assert_eq!(out["metadata"]["from"], "main/scout-1");
    assert_eq!(out["metadata"]["to"], format!("#{dm}"));

    // Root lists channels and sees 1 unread in DM + 1 direct child in team section.
    let list = ListChannelTool::new(Arc::clone(&service));
    let listed = list
        .execute(&tool_ctx(root, allow_all_policy()), json!({}))
        .await
        .unwrap();
    let channels = listed["channels"].as_array().unwrap();
    assert_eq!(channels.len(), 2);
    let dm_row = channels.iter().find(|c| c["id"] == dm).unwrap();
    assert_eq!(dm_row["unread"], 1);
    let group_row = channels.iter().find(|c| c["id"] == group).unwrap();
    assert_eq!(group_row["can_speak"], true);
    assert_eq!(listed["team"].as_array().unwrap().len(), 1);

    // Root reads via ChannelUriSchemaHandler using a member-handle alias (`channel://scout-1`).
    let uri = ChannelUriSchemaHandler::new(Arc::clone(&service));
    let resolved = uri
        .read(
            UriResolveCtx {
                session: Some(root),
                actor_claim: None,
                workdir: Path::new("."),
                runtime: Some(host.as_ref()),
            },
            "channel://scout-1?last=5",
        )
        .await
        .unwrap();
    assert!(resolved.body.contains("[main/scout-1] status ok"));
    assert!(resolved.body.contains("[warning]"));
    assert_eq!(resolved.metadata.as_ref().unwrap()["channel"], dm);
    // Reading showed all unread mail -> notified host with mark=7, incoming=true.
    assert_eq!(*host.notified.lock().unwrap(), vec![(root, 7, true)]);

    // Subsequent list_channel shows 0 unread.
    let listed_after = list
        .execute(&tool_ctx(root, allow_all_policy()), json!({}))
        .await
        .unwrap();
    let dm_after = listed_after["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == dm)
        .unwrap();
    assert_eq!(dm_after["unread"], 0);
}

#[tokio::test]
async fn send_enforces_policy_user_stop_claim_and_revival() {
    let (host, root, child, _group, dm) = setup_team();
    let service = Arc::new(AgentChannelsService::with_runtime(
        Arc::clone(&host) as Arc<dyn HostRuntime>
    ));
    let send = SendTool::new(Arc::clone(&service));

    // 1. Policy denies send -> rejected.
    let denied = ChannelPolicySnapshot::default();
    let err = send
        .execute(
            &tool_ctx(root, denied),
            json!({ "channel": dm, "body": "blocked" }),
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("direction denied by policy"),
        "{err}"
    );

    // 2. User-stopped guard -> rejected.
    host.stopped_by_user
        .lock()
        .unwrap()
        .push((root, "main/scout-1".to_string()));
    let err = send
        .execute(
            &tool_ctx(root, allow_all_policy()),
            json!({ "channel": dm, "body": "wake up" }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("the user stopped"), "{err}");
    host.stopped_by_user.lock().unwrap().clear();

    // 3. Missing resident actor claim -> rejected.
    host.claims.lock().unwrap().clear();
    let err = send
        .execute(
            &tool_ctx(root, allow_all_policy()),
            json!({ "channel": dm, "body": "no claim" }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no active actor claim"), "{err}");

    // 4. Archived direct child -> invokes host revival callback.
    host.push_extension(
        root,
        ExtensionEvent::AgentArchived {
            session: root,
            handle: "main/scout-1".to_string(),
            child,
            reason: hya_tool::ArchiveReason::Reported,
        },
    );
    let out = send
        .execute(
            &tool_ctx(root, allow_all_policy()),
            json!({ "channel": dm, "body": "revive now" }),
        )
        .await
        .unwrap();
    assert_eq!(out["metadata"]["recipients"], 1);
    let revivals = host.revivals.lock().unwrap();
    assert_eq!(revivals.len(), 1);
    assert_eq!(revivals[0].target_handle, "main/scout-1");
    assert_eq!(revivals[0].body, "revive now");
}

#[tokio::test]
async fn unread_mail_steer_hook_splices_notice_and_stages_atomic_ack() {
    let (host, root, child, _group, dm) = setup_team();
    host.marks.lock().unwrap().insert(root, 42);
    let service = Arc::new(AgentChannelsService::with_runtime(
        Arc::clone(&host) as Arc<dyn HostRuntime>
    ));
    let hook = UnreadMailSteerHook::new(Arc::clone(&service));

    // Deliver a DM from child to root.
    AgentChannelsService::send(
        host.as_ref(),
        child,
        MailEndpoint::Channel(dm.clone()),
        MailKind::Message,
        "found root cause".to_string(),
        None,
        Some(allow_all_policy()),
        false,
    )
    .await
    .unwrap();

    // 1. `wait` is skipped (leaves wait_ack untouched).
    let wait_out = hook
        .tool_execute_after(NativeToolAfterInput {
            session: root,
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "wait".to_string(),
            input: json!({}),
            result: NativeToolResult::Ok {
                output: json!({"output": "waited"}),
                time_ms: 5,
            },
        })
        .await
        .unwrap();
    let NativeToolAfterOutcome::Continue {
        result: NativeToolResult::Ok { output, .. },
    } = wait_out
    else {
        panic!("expected Ok");
    };
    assert_eq!(output["output"], "waited");
    assert!(host.staged_acks.lock().unwrap().is_empty());

    // 2. Suppressed session skips steering.
    host.steer_suppressed.lock().unwrap().insert(root, true);
    let suppressed_out = hook
        .tool_execute_after(NativeToolAfterInput {
            session: root,
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "bash".to_string(),
            input: json!({}),
            result: NativeToolResult::Ok {
                output: json!({"output": "ok"}),
                time_ms: 5,
            },
        })
        .await
        .unwrap();
    let NativeToolAfterOutcome::Continue {
        result: NativeToolResult::Ok { output, .. },
    } = suppressed_out
    else {
        panic!("expected Ok");
    };
    assert_eq!(output["output"], "ok");
    host.steer_suppressed.lock().unwrap().insert(root, false);

    // 3. Normal tool call receives `[NEW MAIL]` notice AND stages `StagedMailAck`.
    let steered = hook
        .tool_execute_after(NativeToolAfterInput {
            session: root,
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "bash".to_string(),
            input: json!({}),
            result: NativeToolResult::Ok {
                output: json!({"output": "done"}),
                time_ms: 5,
            },
        })
        .await
        .unwrap();
    let NativeToolAfterOutcome::Continue {
        result: NativeToolResult::Ok { output, .. },
    } = steered
    else {
        panic!("expected Ok");
    };
    let text = output["output"].as_str().unwrap();
    assert!(text.contains("[NEW MAIL"), "{text}");
    assert!(text.contains("found root cause"), "{text}");
    assert!(text.contains(&format!("@{dm}")), "{text}");
    let ack = host.staged_acks.lock().unwrap().remove(&root).unwrap();
    assert_eq!(ack.root, root);
    assert_eq!(ack.mark, 42);
    assert!(ack.consumed_incoming);
}

#[tokio::test]
async fn bundle_api_serves_read_only_channels_messages_inbox_and_team_status() {
    let (host, root, child, _group, dm) = setup_team();
    let service = Arc::new(AgentChannelsService::with_runtime(
        Arc::clone(&host) as Arc<dyn HostRuntime>
    ));
    AgentChannelsService::send(
        host.as_ref(),
        child,
        MailEndpoint::Channel(dm.clone()),
        MailKind::Message,
        "peekable message".to_string(),
        None,
        Some(allow_all_policy()),
        false,
    )
    .await
    .unwrap();

    let api = AgentChannelsBundleApiHandler::new(Arc::clone(&service));
    assert_eq!(api.endpoints().len(), 4);

    // GET /channels/{channel}/messages is read-only (does NOT consume unread cursor).
    let mut path_params = BTreeMap::new();
    path_params.insert("channel".to_string(), dm.clone());
    let reply = api
        .handle(NativeBundleApiRequest {
            api: "channels.messages".to_string(),
            method: ApiMethod::Get,
            scope: ApiScope::Session,
            path: format!("/channels/{dm}/messages"),
            path_params,
            query: BTreeMap::new(),
            body: Value::Null,
            session: Some(root),
        })
        .await
        .unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body["messages"][0]["body"], "peekable message");
    assert_eq!(reply.body["unread_remaining"], 1);

    // GET /inbox confirms unread message is still unread after peek!
    let inbox_reply = api
        .handle(NativeBundleApiRequest {
            api: "channels.inbox".to_string(),
            method: ApiMethod::Get,
            scope: ApiScope::Session,
            path: "/inbox".to_string(),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            body: Value::Null,
            session: Some(root),
        })
        .await
        .unwrap();
    assert_eq!(inbox_reply.status, 200);
    assert_eq!(inbox_reply.body["unread"].as_array().unwrap().len(), 1);
    let team = team_projection(&host.fold(root));
    assert_eq!(team.roster["main"].resident_cursor, 0);
}

#[test]
fn channel_read_tool_uri_parser_and_policy_helpers() {
    assert_eq!(
        parse_channel_uri("channel://DM-12345678?last=10").unwrap(),
        ("DM-12345678".to_string(), Some(10))
    );
    assert!(parse_channel_uri("channel://").is_err());

    let mut val = json!({"output": "hello"});
    append_steer_notice(&mut val, "\n\nmail");
    assert_eq!(val["output"], "hello\n\nmail");

    let empty_policy = ChannelPolicy::empty();
    assert!(!empty_policy.allows_unit(
        ChannelCapability::Send,
        "build",
        ChannelParticipantRole::UnitLeader
    ));

    // Verify default policy from empty catalog (loads first-party defaults if available).
    let _ = ChannelPolicy::from_bundle_catalog(&BundleCatalog::from_prepared(&[]).unwrap());

    // Verify ChannelReadTool accepts `#` and `channel://` and rejects file paths.
    struct Echo;
    #[async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "read"
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new("read"),
                description: String::new(),
                input_schema: json!({}),
                output_schema: None,
            }
        }
        async fn execute(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
            Ok(input["path"].clone())
        }
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let tool = ChannelReadTool::new(Arc::new(Echo));
    let ctx = tool_ctx(SessionId::new(), allow_all_policy());
    let out = rt
        .block_on(tool.execute(&ctx, json!({"path": "#DM-12345678"})))
        .unwrap();
    assert_eq!(out, "channel://#DM-12345678");
    assert!(
        rt.block_on(tool.execute(&ctx, json!({"path": "src/lib.rs"})))
            .is_err()
    );
}
