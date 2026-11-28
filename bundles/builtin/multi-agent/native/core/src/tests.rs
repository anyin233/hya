#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_bundle::{ApiMethod, ApiScope, Bundle, PreparedAgent};
use hya_proto::{
    ActorClaim, ActorEpoch, AgentName, Envelope, Event, EventSeq, FinishReason, MailEndpoint,
    MemberId, MemberRunStatus, Message, MessageId, ModelRef, OwnerRunId, PartId, Projection,
    ProjectionReducerRegistry, Role, SessionId, SessionKind, SubagentMode, ToolCallId, now_millis,
};
use hya_runtime_abi::{
    BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
    BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionEventContext, ExtensionKind,
    HostBundleStorage, HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry,
    HostModelCaller, HostRuntime, HostSessionStore, HostTurnRunner, ModelCallRequest,
    ModelCallResponse, ModelStreamChunk, ModelWindowInfo, NativeBundleApiHandler as _,
    NativeBundleApiRequest, NativeHook as _, NativeService as _, NativeSessionLifecycleInput,
    ProjectionReducer as _, SessionListFilter, SessionSummaryMeta, TurnOutcome, TurnSpawnRequest,
    TurnStartRequest, TurnStatus,
};
use hya_tool::extension::{BUNDLE_MULTI_AGENT, ExtensionEvent, MultiAgentExtensionState};
use hya_tool::{
    AgentDef, ArchiveReason, ArchiveReceipt, MailReceipt, PermissionModel, PermissionPlane,
    PermissionRules, ReportOutcome, Tool, ToolCtx, WaitMode, WaitSpec, WaitWake,
};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    ArchiveTool, HandleNamer, HandleRng, ListAgentsTool, MAX_SUBAGENT_DEPTH, MULTI_AGENT_BUNDLE_ID,
    MultiAgentBundleApiHandler, MultiAgentLifecycleHook, MultiAgentProjectionReducer,
    MultiAgentService, SearchAgentTool, SubagentGovernor, SubagentLimits, TaskTool, TeamCheckpoint,
    TerminalHandoffOptions, acquire_actor_claim, build_handoff_messages,
    decode_multi_agent_projection, generate_terminal_handoff, list_active_actor_claims,
    load_team_checkpoint, mint_leaf, operator_names, read_actor_claim, recover_stale_actor_claims,
    release_actor_claim, renew_actor_claim, resolve_member_target, save_team_checkpoint,
    team_leaves,
};

struct MockHost {
    owner: OwnerRunId,
    bus: broadcast::Sender<Envelope>,
    events: Mutex<HashMap<SessionId, Vec<Envelope>>>,
    state: Mutex<HashMap<(String, String, String), BundleStateEntry>>,
    claims: Mutex<HashMap<(String, String), BundleClaimLease>>,
    model_reply: Mutex<Option<Result<String, HostError>>>,
    archive_result: Mutex<Option<Result<ArchiveReceipt, HostError>>>,
    steer_result: Mutex<Option<Result<MailReceipt, HostError>>>,
    bundle_cfg: Mutex<Value>,
}

impl MockHost {
    fn new() -> Self {
        let (bus, _) = broadcast::channel(64);
        Self {
            owner: OwnerRunId::new(),
            bus,
            events: Mutex::new(HashMap::new()),
            state: Mutex::new(HashMap::new()),
            claims: Mutex::new(HashMap::new()),
            model_reply: Mutex::new(None),
            archive_result: Mutex::new(None),
            steer_result: Mutex::new(None),
            bundle_cfg: Mutex::new(Value::Null),
        }
    }

    fn seed_session(&self, session: SessionId, parent: Option<SessionId>, agent: &str) {
        self.push_event(
            session,
            Event::SessionCreated {
                session,
                parent,
                agent: AgentName::new(agent),
                model: ModelRef::new("fake/model"),
                workdir: "/tmp".to_string(),
                project: None,
                kind: SessionKind::Project,
            },
        );
    }

    fn push_event(&self, session: SessionId, event: Event) -> Envelope {
        let mut map = self.events.lock().unwrap();
        let log = map.entry(session).or_default();
        let seq = EventSeq((log.len() as u64) + 1);
        let env = Envelope {
            seq,
            ts_millis: now_millis(),
            event,
        };
        log.push(env.clone());
        let _ = self.bus.send(env.clone());
        env
    }

    fn fold_projection(&self, session: SessionId) -> Projection {
        let envelopes = self
            .events
            .lock()
            .unwrap()
            .get(&session)
            .cloned()
            .unwrap_or_default();
        let mut reducers = ProjectionReducerRegistry::new();
        reducers = reducers.with_reducer(Arc::new(LocalReducerBridge));
        let mut proj = Projection::default();
        for env in &envelopes {
            proj.apply_with(env, &reducers);
        }
        proj
    }
}

struct LocalReducerBridge;

impl hya_proto::ProjectionReducer for LocalReducerBridge {
    fn bundle_id(&self) -> &str {
        BUNDLE_MULTI_AGENT
    }

    fn fold_extension(
        &self,
        projection: &mut Projection,
        session: SessionId,
        kind: &str,
        payload: &Value,
    ) {
        let reducer = MultiAgentProjectionReducer;
        let state = projection
            .extensions
            .entry(BUNDLE_MULTI_AGENT.to_string())
            .or_insert_with(|| reducer.initial_state());
        let ctx = ExtensionEventContext {
            session,
            seq: EventSeq(projection.last_seq),
            ts_millis: 0,
            bundle: BUNDLE_MULTI_AGENT.to_string(),
            kind: kind.to_string(),
            payload: payload.clone(),
        };
        let _ = reducer.apply(state, &ctx);
    }
}

#[async_trait]
impl HostSessionStore for MockHost {
    async fn session_exists(&self, session: SessionId) -> Result<bool, HostError> {
        Ok(self.events.lock().unwrap().contains_key(&session))
    }

    async fn read_events(
        &self,
        session: SessionId,
        after_seq: Option<EventSeq>,
        limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError> {
        let mut list = self
            .events
            .lock()
            .unwrap()
            .get(&session)
            .cloned()
            .unwrap_or_default();
        if let Some(after) = after_seq {
            list.retain(|e| e.seq > after);
        }
        if let Some(max) = limit {
            list.truncate(max);
        }
        Ok(list)
    }

    async fn append_events(
        &self,
        session: SessionId,
        claim: Option<ActorClaim>,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, HostError> {
        if let Some(claim) = claim {
            let claims = self.claims.lock().unwrap();
            let active = claims.get(&(
                MULTI_AGENT_BUNDLE_ID.to_string(),
                claim.actor_id.to_string(),
            ));
            let valid = active.is_some_and(|lease| {
                lease.epoch == claim.epoch.get()
                    && lease.owner_id == claim.owner_run_id.as_uuid().to_string()
            });
            if !valid {
                return Err(HostError::StaleClaim("fenced actor claim rejected".into()));
            }
        }
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            out.push(self.push_event(session, event));
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
        let mut res = self
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
        Ok(res.pop().unwrap())
    }

    async fn read_projection(&self, session: SessionId) -> Result<Arc<Projection>, HostError> {
        Ok(Arc::new(self.fold_projection(session)))
    }

    async fn read_extension_state(
        &self,
        session: SessionId,
        bundle: &str,
    ) -> Result<Option<Value>, HostError> {
        Ok(self
            .fold_projection(session)
            .extensions
            .get(bundle)
            .cloned())
    }

    async fn session_metadata(
        &self,
        session: SessionId,
    ) -> Result<Option<SessionSummaryMeta>, HostError> {
        let proj = self.fold_projection(session);
        if proj.last_seq == 0 {
            return Ok(None);
        }
        Ok(Some(SessionSummaryMeta {
            session,
            parent: proj.session.parent,
            agent: proj.session.agent.clone(),
            model: proj.session.model.clone(),
            workdir: proj.session.workdir.clone(),
            title: proj.session.title.clone(),
            project: None,
            archived: proj.session.archived.is_some(),
            metadata: Value::Null,
            event_count: proj.last_seq,
        }))
    }

    async fn list_sessions(
        &self,
        _filter: SessionListFilter,
    ) -> Result<Vec<SessionSummaryMeta>, HostError> {
        Ok(Vec::new())
    }

    fn runtime_owner_id(&self) -> Option<OwnerRunId> {
        Some(self.owner)
    }
}

#[async_trait]
impl HostBundleStorage for MockHost {
    async fn get_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
    ) -> Result<Option<BundleStateEntry>, HostError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .get(&(bundle.to_string(), scope.to_string(), key.to_string()))
            .cloned())
    }

    async fn put_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
        value: Value,
        _expected_version: Option<u64>,
    ) -> Result<BundleStateEntry, HostError> {
        let entry = BundleStateEntry {
            bundle: bundle.to_string(),
            scope: scope.to_string(),
            key: key.to_string(),
            value,
            version: 1,
            updated_at_millis: now_millis(),
        };
        self.state.lock().unwrap().insert(
            (bundle.to_string(), scope.to_string(), key.to_string()),
            entry.clone(),
        );
        Ok(entry)
    }

    async fn delete_state(&self, bundle: &str, scope: &str, key: &str) -> Result<bool, HostError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .remove(&(bundle.to_string(), scope.to_string(), key.to_string()))
            .is_some())
    }

    async fn list_state(
        &self,
        bundle: &str,
        scope: &str,
        prefix: Option<&str>,
    ) -> Result<Vec<BundleStateEntry>, HostError> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .values()
            .filter(|e| {
                e.bundle == bundle
                    && e.scope == scope
                    && prefix.is_none_or(|p| e.key.starts_with(p))
            })
            .cloned()
            .collect())
    }

    async fn acquire_claim(
        &self,
        request: BundleClaimRequest,
    ) -> Result<BundleClaimLease, HostError> {
        let mut map = self.claims.lock().unwrap();
        let key = (request.bundle.clone(), request.resource_key.clone());
        let next_epoch = map.get(&key).map_or(1, |old| old.epoch + 1);
        let lease = BundleClaimLease {
            bundle: request.bundle,
            resource_key: request.resource_key,
            owner_id: request.owner_id,
            epoch: next_epoch,
            expires_at_millis: if request.ttl_millis == 0 {
                i64::MAX
            } else {
                now_millis().saturating_add(request.ttl_millis as i64)
            },
            metadata: request.metadata,
        };
        map.insert(key, lease.clone());
        Ok(lease)
    }

    async fn renew_claim(
        &self,
        lease: &BundleClaimLease,
        ttl_millis: u64,
    ) -> Result<BundleClaimLease, HostError> {
        let mut map = self.claims.lock().unwrap();
        let key = (lease.bundle.clone(), lease.resource_key.clone());
        let current = map
            .get_mut(&key)
            .ok_or_else(|| HostError::StaleClaim("claim missing".into()))?;
        if current.epoch != lease.epoch || current.owner_id != lease.owner_id {
            return Err(HostError::StaleClaim("claim superseded".into()));
        }
        current.expires_at_millis = if ttl_millis == 0 {
            i64::MAX
        } else {
            now_millis().saturating_add(ttl_millis as i64)
        };
        Ok(current.clone())
    }

    async fn release_claim(&self, lease: &BundleClaimLease) -> Result<(), HostError> {
        let mut map = self.claims.lock().unwrap();
        let key = (lease.bundle.clone(), lease.resource_key.clone());
        let matches = map
            .get(&key)
            .is_some_and(|cur| cur.epoch == lease.epoch && cur.owner_id == lease.owner_id);
        if !matches {
            return Err(HostError::StaleClaim("claim superseded".into()));
        }
        map.remove(&key);
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

    async fn list_claims(&self, bundle: &str) -> Result<Vec<BundleClaimLease>, HostError> {
        Ok(self
            .claims
            .lock()
            .unwrap()
            .values()
            .filter(|c| c.bundle == bundle)
            .cloned()
            .collect())
    }
}

#[async_trait]
impl HostTurnRunner for MockHost {
    async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
        Ok(SessionId::new())
    }

    async fn run_turn(
        &self,
        request: TurnStartRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Ok(TurnOutcome {
            session: request.session,
            message_id: None,
            finish_reason: Some(FinishReason::Stop),
            output_text: String::new(),
            error: None,
        })
    }

    async fn spawn_session_and_run(
        &self,
        _request: TurnSpawnRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Ok(TurnOutcome {
            session: SessionId::new(),
            message_id: None,
            finish_reason: Some(FinishReason::Stop),
            output_text: String::new(),
            error: None,
        })
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

    async fn archive_subagent(&self, _child: SessionId) -> Result<ArchiveReceipt, HostError> {
        self.archive_result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                Err(HostError::InvalidInput(
                    "subagents are not supervised by this process".into(),
                ))
            })
    }

    async fn steer_subagent(
        &self,
        _child: SessionId,
        _text: &str,
    ) -> Result<MailReceipt, HostError> {
        self.steer_result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| {
                Err(HostError::PermissionDenied(
                    "mail authorization rejected".into(),
                ))
            })
    }
}

#[async_trait]
impl HostModelCaller for MockHost {
    async fn complete(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelCallResponse, HostError> {
        match self.model_reply.lock().unwrap().clone() {
            Some(Ok(text)) => Ok(ModelCallResponse {
                model: ModelRef::new("fake/model"),
                text,
                reasoning: None,
                tool_calls: Vec::new(),
                finish_reason: FinishReason::Stop,
                usage: None,
            }),
            Some(Err(err)) => Err(err),
            None => Err(HostError::Unavailable("no mock model reply".into())),
        }
    }

    async fn stream(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
        Err(HostError::Unavailable("stream unused".into()))
    }

    async fn compact_window(
        &self,
        _model: &ModelRef,
        _messages: &[Message],
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
        let _ = self.bus.send(envelope);
        Ok(())
    }

    fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
        Ok(self.bus.subscribe())
    }

    async fn publish_extension(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), HostError> {
        let _ = self
            .append_extension_event(session, bundle, kind, payload, None)
            .await?;
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
    ) -> Result<Vec<hya_tool::SkillCatalogEntry>, HostError> {
        Ok(Vec::new())
    }
    async fn list_bundles(&self) -> Result<Vec<BundleCatalogSummary>, HostError> {
        Ok(Vec::new())
    }
    async fn get_bundle(&self, _id: &str) -> Result<Option<Bundle>, HostError> {
        Ok(None)
    }
    async fn bundle_config(&self, _bundle_id: &str) -> Result<Value, HostError> {
        Ok(self.bundle_cfg.lock().unwrap().clone())
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
        Ok(true)
    }
    async fn invoke_bundle_api(
        &self,
        _call: BundleApiInvocation,
    ) -> Result<BundleApiResult, HostError> {
        Err(HostError::Unavailable("unused".into()))
    }
    async fn invoke_tool(
        &self,
        _session: Option<SessionId>,
        _tool_name: &str,
        _input: Value,
        _cancel: CancellationToken,
    ) -> Result<Value, HostError> {
        Err(HostError::Unavailable("unused".into()))
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

#[test]
fn governor_enforces_clamped_concurrency_and_fixed_depth() {
    assert_eq!(MAX_SUBAGENT_DEPTH, 2);
    let clamped_low = SubagentGovernor::new(SubagentLimits {
        max_concurrency: 0,
        ..SubagentLimits::default()
    });
    assert_eq!(clamped_low.limits().max_concurrency, 1);
    assert_eq!(clamped_low.max_depth(), 2);

    let clamped_high = SubagentGovernor::new(SubagentLimits {
        max_concurrency: 999,
        ..SubagentLimits::default()
    });
    assert_eq!(clamped_high.limits().max_concurrency, 100);
    assert_eq!(clamped_high.available_general_stream_permits(), 100);
    assert_eq!(clamped_high.available_reserved_stream_permits(), 28);
}

#[test]
fn handle_namer_mints_unique_leaves_and_releases_reservations() {
    struct Fixed(usize);
    impl HandleRng for Fixed {
        fn pick(&mut self, bound: usize) -> usize {
            self.0.min(bound - 1)
        }
    }

    let names = operator_names();
    let taken: BTreeSet<String> = [format!("scout-{}", names[0])].into_iter().collect();
    let leaf = mint_leaf("scout", &taken, &mut Fixed(0));
    assert_eq!(leaf, format!("scout-{}-{}", names[0], names[1]));

    let namer = Arc::new(HandleNamer::new(Box::new(Fixed(2))));
    let root = SessionId::new();
    {
        let r1 = namer.reserve(root, "hya-task", BTreeSet::new());
        let r2 = namer.reserve(root, "hya-task", BTreeSet::new());
        assert_ne!(r1.leaf, r2.leaf);
        assert_eq!(namer.reserved_count(root), 2);
    }
    assert_eq!(namer.reserved_count(root), 0);
}

#[test]
fn projection_reducer_folds_full_multi_agent_lifecycle() {
    let reducer = MultiAgentProjectionReducer;
    let root = SessionId::new();
    let child = SessionId::new();
    let member = MemberId::new();
    let mut state = reducer.initial_state();

    let events = vec![
        ExtensionEvent::MemberSpawned {
            session: root,
            member,
            child: Some(child),
            subagent_type: AgentName::new("hya-task"),
            description: "unit task".into(),
            depth: 1,
            directive: "do unit".into(),
            tool_call: Some(ToolCallId::new()),
        },
        ExtensionEvent::AgentRegistered {
            session: root,
            agent_session: child,
            handle: "hya-task-amiya".into(),
            parent: Some("main".into()),
            agent_type: AgentName::new("hya-task"),
            mode: SubagentMode::Resident,
        },
        ExtensionEvent::ResidentWorkStarted {
            session: root,
            actor_session: child,
            handle: "main/hya-task-amiya".into(),
            epoch: ActorEpoch::from_storage(1),
            inbox_through: 3,
        },
        ExtensionEvent::AgentHeartbeat {
            session: root,
            handle: "main/hya-task-amiya".into(),
            heartbeat_ms: 42_000,
        },
        ExtensionEvent::ResidentWorkFinished {
            session: root,
            handle: "main/hya-task-amiya".into(),
            epoch: ActorEpoch::from_storage(1),
            inbox_through: 3,
        },
        ExtensionEvent::SubagentReported {
            session: root,
            member,
            child,
            handle: "main/hya-task-amiya".into(),
            outcome: ReportOutcome::Done,
            report: "finished cleanly".into(),
        },
        ExtensionEvent::HandoffCommitted {
            session: root,
            handle: "main/hya-task-amiya".into(),
            generation: 1,
            doc: "1. Goal - unit\n".into(),
            degraded: false,
        },
        ExtensionEvent::AgentArchived {
            session: root,
            handle: "main/hya-task-amiya".into(),
            child,
            reason: ArchiveReason::Reported,
        },
    ];

    for (idx, ext) in events.into_iter().enumerate() {
        let Event::Extension {
            session,
            bundle,
            kind,
            payload,
        } = ext.into_event()
        else {
            panic!("expected extension event");
        };
        reducer
            .apply(
                &mut state,
                &ExtensionEventContext {
                    session,
                    seq: EventSeq((idx as u64) + 1),
                    ts_millis: 1000,
                    bundle,
                    kind,
                    payload,
                },
            )
            .unwrap();
    }

    let decoded: MultiAgentExtensionState = serde_json::from_value(state).unwrap();
    assert_eq!(decoded.members.len(), 1);
    assert_eq!(decoded.members[0].status, MemberRunStatus::Done);
    assert_eq!(decoded.members[0].summary, "finished cleanly");
    assert!(decoded.roster.is_empty());
    assert!(decoded.archived.contains_key("main/hya-task-amiya"));
    assert_eq!(decoded.handoff.as_ref().unwrap().generation, 1);

    let mut proj = Projection::default();
    proj.extensions.insert(
        BUNDLE_MULTI_AGENT.to_string(),
        serde_json::to_value(&decoded).unwrap(),
    );
    assert_eq!(decode_multi_agent_projection(&proj), decoded);
    assert!(team_leaves(&proj).contains("hya-task-amiya"));
}

#[tokio::test]
async fn actor_claims_fencing_recovery_and_checkpoints_persist_via_host_storage() {
    let host = Arc::new(MockHost::new());
    let root = SessionId::new();
    let actor = SessionId::new();
    let owner1 = OwnerRunId::new();
    let owner2 = OwnerRunId::new();

    let lease1 = acquire_actor_claim(host.as_ref(), root, actor, owner1, 5_000)
        .await
        .unwrap();
    assert_eq!(lease1.claim.epoch.get(), 1);
    let renewed = renew_actor_claim(host.as_ref(), &lease1, 10_000)
        .await
        .unwrap();
    assert_eq!(renewed.claim.epoch.get(), 1);

    // Recovering under a new OwnerRunId bumps the epoch and fences the old owner.
    let recovered = recover_stale_actor_claims(host.as_ref(), owner2)
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    let (lease2, prev_epoch) = &recovered[0];
    assert_eq!(prev_epoch.get(), 1);
    assert_eq!(lease2.claim.epoch.get(), 2);
    assert_eq!(lease2.claim.owner_run_id, owner2);

    // Old lease renewal/release and fenced writes are rejected with StaleClaim.
    assert!(matches!(
        renew_actor_claim(host.as_ref(), &lease1, 5_000).await,
        Err(HostError::StaleClaim(_))
    ));
    assert!(matches!(
        release_actor_claim(host.as_ref(), &lease1).await,
        Err(HostError::StaleClaim(_))
    ));

    let active = list_active_actor_claims(host.as_ref(), Some(root))
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert!(
        read_actor_claim(host.as_ref(), actor)
            .await
            .unwrap()
            .is_some()
    );

    release_actor_claim(host.as_ref(), lease2).await.unwrap();
    assert!(
        read_actor_claim(host.as_ref(), actor)
            .await
            .unwrap()
            .is_none()
    );

    // Team checkpoints round-trip via bundle_state.
    let cp = TeamCheckpoint {
        root,
        owner_run_id: owner2.as_uuid().to_string(),
        resident_turns: 7,
        team_messages: 12,
    };
    save_team_checkpoint(host.as_ref(), &cp).await.unwrap();
    assert_eq!(
        load_team_checkpoint(host.as_ref(), root).await.unwrap(),
        Some(cp)
    );
}

#[tokio::test]
async fn terminal_handoff_supports_model_and_deterministic_degraded_fallback() {
    let host = Arc::new(MockHost::new());
    let session = SessionId::new();
    host.seed_session(session, None, "hya-task");
    let msg_id = MessageId::new();
    host.push_event(
        session,
        Event::MessageStarted {
            session,
            message: msg_id,
            role: Role::User,
            agent: None,
            model: None,
        },
    );
    let part_id = PartId::new();
    host.push_event(
        session,
        Event::TextStart {
            session,
            message: msg_id,
            part: part_id,
        },
    );
    host.push_event(
        session,
        Event::TextDelta {
            session,
            message: msg_id,
            part: part_id,
            delta: "Implement feature X".into(),
        },
    );

    let req_msgs = build_handoff_messages(&[], Some("PRIOR_SUMMARY"));
    assert_eq!(req_msgs.len(), 1);

    // Without a model reply, falls back deterministically with degraded=true.
    let degraded = generate_terminal_handoff(
        host.as_ref(),
        session,
        TerminalHandoffOptions::default(),
        CancellationToken::new(),
    )
    .await;
    assert!(degraded.degraded);
    assert_eq!(degraded.generation, 1);
    assert!(degraded.doc.contains("Implement feature X"));

    // With a model reply, returns non-degraded handoff.
    *host.model_reply.lock().unwrap() =
        Some(Ok("1. Goal - Feature X\n2. Current state - Done".into()));
    let model = generate_terminal_handoff(
        host.as_ref(),
        session,
        TerminalHandoffOptions::default(),
        CancellationToken::new(),
    )
    .await;
    assert!(!model.degraded);
    assert!(model.doc.contains("Feature X"));
}

#[tokio::test]
async fn service_lifecycle_bundle_api_and_wait_work_over_host_runtime() {
    let host = Arc::new(MockHost::new());
    *host.bundle_cfg.lock().unwrap() = json!({
        "subagents": { "max_concurrency": 12, "per_run_budget": 64 }
    });
    let service = Arc::new(MultiAgentService::new());
    assert!(!service.health().await.healthy);
    service.start(host.clone()).await.unwrap();
    assert!(service.health().await.healthy);
    assert_eq!(service.governor().limits().max_concurrency, 12);
    assert_eq!(service.governor().limits().per_run_budget, 64);

    let root = SessionId::new();
    let child = SessionId::new();
    host.seed_session(root, None, "hya-main");
    host.seed_session(child, Some(root), "hya-task");

    let (lease, _member) = service
        .register_resident_actor(
            host.as_ref(),
            root,
            root,
            "main",
            child,
            "hya-task-suzuran",
            AgentName::new("hya-task"),
            "investigate".into(),
            1,
            "check files".into(),
        )
        .await
        .unwrap();

    // Target resolution handles session id, full handle, and leaf.
    let proj = host.read_projection(root).await.unwrap();
    assert_eq!(
        resolve_member_target(&proj, root, "main", "hya-task-suzuran")
            .unwrap()
            .session,
        child
    );

    let api = MultiAgentBundleApiHandler::new(Arc::clone(&service));
    assert_eq!(api.id(), MULTI_AGENT_BUNDLE_ID);
    assert_eq!(api.endpoints().len(), 6);

    // GET /members (session-scoped)
    let members_reply = api
        .handle(NativeBundleApiRequest {
            api: "list_members".into(),
            method: ApiMethod::Get,
            scope: ApiScope::Session,
            path: "/members".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            body: Value::Null,
            session: Some(root),
        })
        .await
        .unwrap();
    assert_eq!(members_reply.status, 200);
    assert_eq!(members_reply.body["members"].as_array().unwrap().len(), 1);
    assert_eq!(members_reply.body["roster"].as_array().unwrap().len(), 1);

    // GET /members (global-scoped via ?session=...)
    let global_members = api
        .handle(NativeBundleApiRequest {
            api: "list_members_global".into(),
            method: ApiMethod::Get,
            scope: ApiScope::Global,
            path: "/members".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::from([("session".into(), root.to_string())]),
            body: Value::Null,
            session: None,
        })
        .await
        .unwrap();
    assert_eq!(global_members.status, 200);
    assert_eq!(global_members.body["members"].as_array().unwrap().len(), 1);

    // POST /archive (unsupervised -> 400 invalid_argument)
    let err_archive = api
        .handle(NativeBundleApiRequest {
            api: "archive_subagent".into(),
            method: ApiMethod::Post,
            scope: ApiScope::Session,
            path: "/archive".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            body: json!({}),
            session: Some(child),
        })
        .await
        .unwrap();
    assert_eq!(err_archive.status, 400);
    assert_eq!(err_archive.body["error"]["code"], json!("invalid_argument"));

    // POST /archive (success)
    *host.archive_result.lock().unwrap() = Some(Ok(ArchiveReceipt {
        handle: "main/hya-task-suzuran".into(),
        session: child,
        cancelled_turn: true,
        descendants: vec![],
    }));
    let ok_archive = api
        .handle(NativeBundleApiRequest {
            api: "archive_subagent".into(),
            method: ApiMethod::Post,
            scope: ApiScope::Session,
            path: "/archive".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            body: json!({}),
            session: Some(child),
        })
        .await
        .unwrap();
    assert_eq!(ok_archive.status, 200);
    assert_eq!(ok_archive.body["handle"], json!("main/hya-task-suzuran"));
    assert_eq!(ok_archive.body["cancelledTurn"], json!(true));

    // POST /steer (empty text -> 400 invalid_argument)
    let empty_steer = api
        .handle(NativeBundleApiRequest {
            api: "steer_subagent".into(),
            method: ApiMethod::Post,
            scope: ApiScope::Session,
            path: "/steer".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            body: json!({ "text": "   " }),
            session: Some(child),
        })
        .await
        .unwrap();
    assert_eq!(empty_steer.status, 400);

    // POST /steer (permission denied -> 403 permission_denied)
    let denied_steer = api
        .handle(NativeBundleApiRequest {
            api: "steer_subagent".into(),
            method: ApiMethod::Post,
            scope: ApiScope::Session,
            path: "/steer".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            body: json!({ "text": "continue" }),
            session: Some(child),
        })
        .await
        .unwrap();
    assert_eq!(denied_steer.status, 403);
    assert_eq!(
        denied_steer.body["error"]["code"],
        json!("permission_denied")
    );

    // POST /steer (success)
    *host.steer_result.lock().unwrap() = Some(Ok(MailReceipt {
        from: "main".into(),
        to: MailEndpoint::Channel("DM-12345678".into()),
        recipients: 1,
    }));
    let ok_steer = api
        .handle(NativeBundleApiRequest {
            api: "steer_subagent".into(),
            method: ApiMethod::Post,
            scope: ApiScope::Session,
            path: "/steer".into(),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            body: json!({ "text": "continue" }),
            session: Some(child),
        })
        .await
        .unwrap();
    assert_eq!(ok_steer.status, 200);
    assert_eq!(ok_steer.body["from"], json!("main"));

    // Archive actor via service and verify wait sees nothing left to wait for.
    let handoff = service
        .commit_actor_archive(
            host.as_ref(),
            &lease,
            "main/hya-task-suzuran",
            ArchiveReason::Reported,
            None,
        )
        .await
        .unwrap();
    assert!(handoff.degraded);
    let wait_out = service
        .wait_for_members(
            host.as_ref(),
            root,
            WaitSpec {
                targets: Vec::new(),
                mode: WaitMode::Any,
                timeout: std::time::Duration::from_millis(50),
                wake_on_mail: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(wait_out.woke_by, WaitWake::NothingToWaitFor);

    // Lifecycle hook clears session state cleanly.
    let hook = MultiAgentLifecycleHook::new(Arc::clone(&service));
    hook.session_end(NativeSessionLifecycleInput { session: root })
        .await
        .unwrap();

    service.stop().await.unwrap();
    assert!(!service.health().await.healthy);
}

#[tokio::test]
async fn native_tools_validate_schemas_and_execute() {
    let list = ListAgentsTool;
    let task = TaskTool;
    let search = SearchAgentTool;
    let archive = ArchiveTool;
    assert_eq!(list.name(), "list_agents");
    assert_eq!(task.name(), "task");
    assert_eq!(search.name(), "search_agent");
    assert_eq!(archive.name(), "archive");

    let permission = PermissionPlane::new(PermissionRules::new(Vec::new()))
        .0
        .with_invocation_model(PermissionModel::Danger);
    let mut ctx = ToolCtx::new(std::env::temp_dir(), permission, CancellationToken::new())
        .with_extension(Arc::<[AgentDef]>::from([AgentDef {
            name: "hya-scout".into(),
            description: Some("Read-only scout".into()),
            category: None,
            mode: "subagent".into(),
            effort: None,
            effort_source: None,
        }]));
    ctx.session = Some(SessionId::new());

    let listed = list.execute(&ctx, json!({})).await.unwrap();
    assert_eq!(listed["metadata"]["count"], json!(1));
    assert!(listed["output"].as_str().unwrap().contains("hya-scout"));

    // Removed `name` field is rejected cleanly.
    let bad_task = task
        .execute(
            &ctx,
            json!({ "name": "legacy", "description": "x", "prompt": "y" }),
        )
        .await
        .unwrap_err();
    assert!(bad_task.to_string().contains("`name` was removed"));
}

#[tokio::test]
async fn search_archived_lists_own_archived_direct_children_by_query() {
    use crate::service::ArchivedAgentRow;

    let host = MockHost::new();
    let root = SessionId::new();
    let worker = SessionId::new();
    let scout = SessionId::new();
    let lead = SessionId::new();
    let grandchild = SessionId::new();
    let stray = SessionId::new();
    host.seed_session(root, None, "hya-main");
    for (session, parent, agent) in [
        (worker, root, "hya-task"),
        (scout, root, "hya-scout"),
        (lead, root, "hya-task"),
        (grandchild, lead, "hya-task"),
        (stray, root, "hya-task"),
    ] {
        host.seed_session(session, Some(parent), agent);
    }
    for (agent_session, handle, parent, agent) in [
        (worker, "hya-task-amiya", "main", "hya-task"),
        (scout, "hya-scout-kaltsit", "main", "hya-scout"),
        (lead, "hya-task-lead", "main", "hya-task"),
        (grandchild, "hya-task-w", "main/hya-task-lead", "hya-task"),
    ] {
        host.push_event(
            root,
            ExtensionEvent::AgentRegistered {
                session: root,
                agent_session,
                handle: handle.into(),
                parent: Some(parent.into()),
                agent_type: AgentName::new(agent),
                mode: SubagentMode::Resident,
            }
            .into_event(),
        );
    }
    for (child, handle) in [
        (worker, "main/hya-task-amiya"),
        (scout, "main/hya-scout-kaltsit"),
        (grandchild, "main/hya-task-lead/hya-task-w"),
    ] {
        host.push_event(
            root,
            ExtensionEvent::AgentArchived {
                session: root,
                handle: handle.into(),
                child,
                reason: ArchiveReason::Reported,
            }
            .into_event(),
        );
    }
    host.push_event(
        worker,
        ExtensionEvent::HandoffCommitted {
            session: worker,
            handle: "main/hya-task-amiya".into(),
            generation: 1,
            doc: "1. Goal\nFix the quartz parser\n2. Current state\nhalf done\n3. Pending tasks\nwire tests\n"
                .into(),
            degraded: false,
        }
        .into_event(),
    );

    let worker_row = ArchivedAgentRow {
        handle: "main/hya-task-amiya".into(),
        agent_type: "hya-task".into(),
        session: worker.to_string(),
        goal: "Goal Fix the quartz parser".into(),
        pending: "Pending tasks wire tests".into(),
        degraded: false,
    };
    let scout_row = ArchivedAgentRow {
        handle: "main/hya-scout-kaltsit".into(),
        agent_type: "hya-scout".into(),
        session: scout.to_string(),
        goal: "(no handoff)".into(),
        pending: String::new(),
        degraded: true,
    };

    // The root (unregistered, so `main`) sees only its own archived children,
    // sorted by handle; the live lead and the grandchild are excluded.
    let all = MultiAgentService::search_archived(&host, root, "  ")
        .await
        .unwrap();
    assert_eq!(all, vec![scout_row.clone(), worker_row.clone()]);
    // The query matches goal/pending digests and the agent type, case-insensitively.
    assert_eq!(
        MultiAgentService::search_archived(&host, root, "QUARTZ")
            .await
            .unwrap(),
        vec![worker_row.clone()]
    );
    assert_eq!(
        MultiAgentService::search_archived(&host, root, "wire")
            .await
            .unwrap(),
        vec![worker_row]
    );
    assert_eq!(
        MultiAgentService::search_archived(&host, root, "scout")
            .await
            .unwrap(),
        vec![scout_row]
    );
    assert!(
        MultiAgentService::search_archived(&host, root, "nothing")
            .await
            .unwrap()
            .is_empty()
    );
    // A live member sees its own archived children only.
    let lead_rows = MultiAgentService::search_archived(&host, lead, "")
        .await
        .unwrap();
    assert_eq!(
        lead_rows
            .iter()
            .map(|row| row.handle.as_str())
            .collect::<Vec<_>>(),
        ["main/hya-task-lead/hya-task-w"]
    );
    // A session with no team handle is rejected.
    assert!(matches!(
        MultiAgentService::search_archived(&host, stray, "").await,
        Err(HostError::InvalidInput(_))
    ));
}
