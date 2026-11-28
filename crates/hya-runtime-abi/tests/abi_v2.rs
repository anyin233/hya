//! Unit and FFI tests for `hya-runtime-abi`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_bundle::{Bundle, BundleIdentity, PreparedAgent};
use hya_proto::{
    ActorClaim, AgentName, Envelope, Event, EventSeq, FinishReason, Message, ModelRef, ProjectId,
    Projection, SessionId, ToolName, ToolSchema,
};
use hya_runtime_abi::{
    ABI_VERSION_V2, AbiError, BundleApiInvocation, BundleApiResult, BundleCatalogSummary,
    BundleClaimLease, BundleClaimRequest, BundleStateEntry, DriverRunOutcome, ExtensionEntry,
    ExtensionEventContext, ExtensionKind, GoalDriver, GoalRunRequest, HookPoint, HostBundleStorage,
    HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry, HostModelCaller,
    HostRuntime, HostSessionStore, HostTurnRunner, LoopDriver, LoopRunRequest, ModelCallRequest,
    ModelCallResponse, ModelStreamChunk, ModelWindowInfo, NativeAbiV2, NativeApiEndpoint,
    NativeBundleApiHandler, NativeBundleApiReply, NativeBundleApiRequest, NativeHook,
    NativePluginContext, NativeRegistrations, NativeService, NativeToolBeforeInput,
    NativeToolBeforeOutcome, ProjectionReducer, ServiceHealth, SessionListFilter,
    SessionSummaryMeta, TurnOutcome, TurnSpawnRequest, TurnStartRequest, TurnStatus, UriContent,
    UriResolveCtx, UriSchemaHandler, abi_digest_v2, export_native_bundle_v2,
};
use hya_tool::{AgentDef, SkillCatalogEntry, Tool, ToolCtx, ToolError};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct InMemoryHostRuntime {
    states: Mutex<HashMap<(String, String, String), BundleStateEntry>>,
    claims: Mutex<HashMap<(String, String), BundleClaimLease>>,
    events: Mutex<Vec<Envelope>>,
    turns_run: Mutex<Vec<String>>,
    bus: InMemoryBus,
}

struct InMemoryBus {
    tx: broadcast::Sender<Envelope>,
}

impl Default for InMemoryBus {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(64);
        Self { tx }
    }
}

#[async_trait]
impl HostSessionStore for InMemoryHostRuntime {
    async fn session_exists(&self, _session: SessionId) -> Result<bool, HostError> {
        Ok(true)
    }

    async fn read_events(
        &self,
        session: SessionId,
        _after_seq: Option<EventSeq>,
        _limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError> {
        let events = self
            .events
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        Ok(events
            .iter()
            .filter(|env| env.event.session() == Some(session))
            .cloned()
            .collect())
    }

    async fn append_events(
        &self,
        _session: SessionId,
        _claim: Option<ActorClaim>,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, HostError> {
        let mut stored = self
            .events
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        let mut out = Vec::new();
        for event in events {
            let seq = EventSeq(u64::try_from(stored.len()).unwrap_or(u64::MAX) + 1);
            let env = Envelope {
                seq,
                ts_millis: 1_700_000_000_000,
                event,
            };
            stored.push(env.clone());
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
        _claim: Option<ActorClaim>,
    ) -> Result<Envelope, HostError> {
        let mut stored = self
            .events
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        let seq = EventSeq(u64::try_from(stored.len()).unwrap_or(u64::MAX) + 1);
        let env = Envelope {
            seq,
            ts_millis: 1_700_000_000_000,
            event: Event::SessionMetadataSet {
                session,
                metadata: json!({ "bundle": bundle, "kind": kind, "payload": payload }),
            },
        };
        stored.push(env.clone());
        Ok(env)
    }

    async fn read_projection(&self, _session: SessionId) -> Result<Arc<Projection>, HostError> {
        Ok(Arc::new(Projection::default()))
    }

    async fn read_extension_state(
        &self,
        _session: SessionId,
        _bundle: &str,
    ) -> Result<Option<Value>, HostError> {
        Ok(None)
    }

    async fn session_metadata(
        &self,
        session: SessionId,
    ) -> Result<Option<SessionSummaryMeta>, HostError> {
        Ok(Some(SessionSummaryMeta {
            session,
            parent: None,
            agent: Some(AgentName::new("build")),
            model: Some(ModelRef::new("dev/echo")),
            workdir: Some("/tmp".to_string()),
            title: Some("test".to_string()),
            project: None::<ProjectId>,
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
impl HostBundleStorage for InMemoryHostRuntime {
    async fn get_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
    ) -> Result<Option<BundleStateEntry>, HostError> {
        let states = self
            .states
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        Ok(states
            .get(&(bundle.to_string(), scope.to_string(), key.to_string()))
            .cloned())
    }

    async fn put_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
        value: Value,
        expected_version: Option<u64>,
    ) -> Result<BundleStateEntry, HostError> {
        let mut states = self
            .states
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        let map_key = (bundle.to_string(), scope.to_string(), key.to_string());
        let current_version = states.get(&map_key).map_or(0, |entry| entry.version);
        if let Some(expected) = expected_version
            && expected != current_version
        {
            return Err(HostError::Conflict(format!(
                "expected version {expected}, found {current_version}"
            )));
        }
        let entry = BundleStateEntry {
            bundle: bundle.to_string(),
            scope: scope.to_string(),
            key: key.to_string(),
            value,
            version: current_version + 1,
            updated_at_millis: 1_700_000_000_000,
        };
        states.insert(map_key, entry.clone());
        Ok(entry)
    }

    async fn delete_state(&self, bundle: &str, scope: &str, key: &str) -> Result<bool, HostError> {
        let mut states = self
            .states
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        Ok(states
            .remove(&(bundle.to_string(), scope.to_string(), key.to_string()))
            .is_some())
    }

    async fn list_state(
        &self,
        bundle: &str,
        scope: &str,
        prefix: Option<&str>,
    ) -> Result<Vec<BundleStateEntry>, HostError> {
        let states = self
            .states
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        Ok(states
            .iter()
            .filter(|((b, s, k), _)| {
                b == bundle && s == scope && prefix.is_none_or(|p| k.starts_with(p))
            })
            .map(|(_, entry)| entry.clone())
            .collect())
    }

    async fn acquire_claim(
        &self,
        request: BundleClaimRequest,
    ) -> Result<BundleClaimLease, HostError> {
        let mut claims = self
            .claims
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        let key = (request.bundle.clone(), request.resource_key.clone());
        let next_epoch = claims.get(&key).map_or(1, |c| c.epoch + 1);
        let lease = BundleClaimLease {
            bundle: request.bundle,
            resource_key: request.resource_key,
            owner_id: request.owner_id,
            epoch: next_epoch,
            expires_at_millis: 1_700_000_000_000 + i64::try_from(request.ttl_millis).unwrap_or(0),
            metadata: request.metadata,
        };
        claims.insert(key, lease.clone());
        Ok(lease)
    }

    async fn renew_claim(
        &self,
        lease: &BundleClaimLease,
        ttl_millis: u64,
    ) -> Result<BundleClaimLease, HostError> {
        let mut claims = self
            .claims
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        let key = (lease.bundle.clone(), lease.resource_key.clone());
        let Some(existing) = claims.get_mut(&key) else {
            return Err(HostError::StaleClaim("claim does not exist".to_string()));
        };
        if existing.epoch != lease.epoch || existing.owner_id != lease.owner_id {
            return Err(HostError::StaleClaim("claim epoch mismatch".to_string()));
        }
        existing.expires_at_millis += i64::try_from(ttl_millis).unwrap_or(0);
        Ok(existing.clone())
    }

    async fn release_claim(&self, lease: &BundleClaimLease) -> Result<(), HostError> {
        let mut claims = self
            .claims
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        let key = (lease.bundle.clone(), lease.resource_key.clone());
        let Some(existing) = claims.get(&key) else {
            return Err(HostError::StaleClaim("claim does not exist".to_string()));
        };
        if existing.epoch != lease.epoch || existing.owner_id != lease.owner_id {
            return Err(HostError::StaleClaim("claim epoch mismatch".to_string()));
        }
        claims.remove(&key);
        Ok(())
    }

    async fn read_claim(
        &self,
        bundle: &str,
        resource_key: &str,
    ) -> Result<Option<BundleClaimLease>, HostError> {
        let claims = self
            .claims
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        Ok(claims
            .get(&(bundle.to_string(), resource_key.to_string()))
            .cloned())
    }
}

#[async_trait]
impl HostTurnRunner for InMemoryHostRuntime {
    async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
        Ok(SessionId::new())
    }

    async fn run_turn(
        &self,
        request: TurnStartRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        self.turns_run
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?
            .push(request.prompt.clone());
        Ok(TurnOutcome {
            session: request.session,
            message_id: None,
            finish_reason: Some(FinishReason::Stop),
            output_text: format!("ran: {}", request.prompt),
            error: None,
        })
    }

    async fn spawn_session_and_run(
        &self,
        request: TurnSpawnRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Ok(TurnOutcome {
            session: SessionId::new(),
            message_id: None,
            finish_reason: Some(FinishReason::Stop),
            output_text: format!("spawned: {}", request.prompt),
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
        Ok(true)
    }

    async fn turn_status(&self, _session: SessionId) -> Result<TurnStatus, HostError> {
        Ok(TurnStatus::Idle)
    }
}

#[async_trait]
impl HostModelCaller for InMemoryHostRuntime {
    async fn complete(
        &self,
        request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelCallResponse, HostError> {
        Ok(ModelCallResponse {
            model: request.model.unwrap_or_else(|| ModelRef::new("dev/echo")),
            text: "ok".to_string(),
            reasoning: None,
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Stop,
            usage: None,
        })
    }

    async fn stream(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
        let (tx, rx) = mpsc::channel(4);
        let _ = tx
            .send(Ok(ModelStreamChunk::TextDelta("hi".to_string())))
            .await;
        let _ = tx
            .send(Ok(ModelStreamChunk::Finished(FinishReason::Stop)))
            .await;
        Ok(rx)
    }

    async fn compact_window(
        &self,
        _model: &ModelRef,
        _messages: &[Message],
    ) -> Result<Option<String>, HostError> {
        Ok(Some("compacted".to_string()))
    }

    async fn model_window(&self, _model: &ModelRef) -> Result<Option<ModelWindowInfo>, HostError> {
        Ok(Some(ModelWindowInfo {
            max_context: 128_000,
            usage_reporting: true,
            sends_tool_images: false,
        }))
    }
}

#[async_trait]
impl HostEventBus for InMemoryHostRuntime {
    fn publish(&self, envelope: Envelope) -> Result<(), HostError> {
        let _ = self.bus.tx.send(envelope);
        Ok(())
    }

    fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
        Ok(self.bus.tx.subscribe())
    }

    async fn publish_extension(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), HostError> {
        let env = Envelope {
            seq: EventSeq(0),
            ts_millis: 1_700_000_000_000,
            event: Event::SessionMetadataSet {
                session,
                metadata: json!({ "bundle": bundle, "kind": kind, "payload": payload }),
            },
        };
        let _ = self.bus.tx.send(env);
        Ok(())
    }
}

#[async_trait]
impl HostCatalogReader for InMemoryHostRuntime {
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
        Ok(vec![BundleCatalogSummary {
            identity: BundleIdentity {
                id: "hya/test".to_string(),
                version: "0.51.0".to_string(),
                publisher: "hya".to_string(),
            },
            enabled: true,
            description: Some("test bundle".to_string()),
        }])
    }

    async fn get_bundle(&self, _id: &str) -> Result<Option<Bundle>, HostError> {
        Ok(None)
    }

    async fn bundle_config(&self, _bundle_id: &str) -> Result<Value, HostError> {
        Ok(json!({}))
    }

    async fn global_config(&self) -> Result<Value, HostError> {
        Ok(json!({ "schema_version": 2 }))
    }
}

#[async_trait]
impl HostExtensionRegistry for InMemoryHostRuntime {
    async fn list_extensions(
        &self,
        _kind: Option<ExtensionKind>,
    ) -> Result<Vec<ExtensionEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn has_bundle(&self, bundle_id: &str) -> Result<bool, HostError> {
        Ok(bundle_id == "hya/test")
    }

    async fn invoke_bundle_api(
        &self,
        _call: BundleApiInvocation,
    ) -> Result<BundleApiResult, HostError> {
        Ok(BundleApiResult {
            status: 200,
            body: json!({ "ok": true }),
        })
    }

    async fn invoke_tool(
        &self,
        _session: Option<SessionId>,
        tool_name: &str,
        input: Value,
        _cancel: CancellationToken,
    ) -> Result<Value, HostError> {
        Ok(json!({ "tool": tool_name, "input": input }))
    }
}

impl HostRuntime for InMemoryHostRuntime {
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

struct MacroTool;

#[async_trait]
impl Tool for MacroTool {
    fn name(&self) -> &str {
        "macro_tool"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("macro_tool"),
            description: "macro test tool".to_string(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        Ok(input)
    }
}

struct MacroHook;

#[async_trait]
impl NativeHook for MacroHook {
    fn id(&self) -> &str {
        "macro-hook"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::ToolExecuteBefore]
    }

    async fn tool_execute_before(
        &self,
        input: NativeToolBeforeInput,
    ) -> Result<NativeToolBeforeOutcome, HostError> {
        Ok(NativeToolBeforeOutcome::Continue { input: input.input })
    }
}

struct MacroService;

#[async_trait]
impl NativeService for MacroService {
    fn id(&self) -> &str {
        "macro-svc"
    }

    async fn start(&self, _runtime: Arc<dyn HostRuntime>) -> Result<(), HostError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), HostError> {
        Ok(())
    }

    async fn health(&self) -> ServiceHealth {
        ServiceHealth::healthy("ok")
    }
}

struct MacroApi;

#[async_trait]
impl NativeBundleApiHandler for MacroApi {
    fn id(&self) -> &str {
        "macro-api"
    }

    fn endpoints(&self) -> &[NativeApiEndpoint] {
        &[]
    }

    async fn handle(
        &self,
        _request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError> {
        Ok(NativeBundleApiReply {
            status: 204,
            body: Value::Null,
        })
    }
}

struct MacroReducer;

impl ProjectionReducer for MacroReducer {
    fn bundle_id(&self) -> &str {
        "hya/macro"
    }

    fn apply(&self, state: &mut Value, event: &ExtensionEventContext) -> Result<(), HostError> {
        *state = json!({ "last_kind": event.kind });
        Ok(())
    }
}

struct MacroUri;

#[async_trait]
impl UriSchemaHandler for MacroUri {
    fn scheme(&self) -> &str {
        "macro"
    }

    async fn read(&self, _ctx: UriResolveCtx<'_>, uri: &str) -> Result<UriContent, HostError> {
        Ok(UriContent {
            body: uri.to_string(),
            media_type: "text/plain".to_string(),
            metadata: None,
        })
    }
}

struct MacroGoal;

#[async_trait]
impl GoalDriver for MacroGoal {
    fn id(&self) -> &str {
        "macro-goal"
    }

    async fn run_goal(
        &self,
        _runtime: Arc<dyn HostRuntime>,
        request: GoalRunRequest,
        _cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError> {
        Ok(DriverRunOutcome::Achieved {
            iterations: 1,
            reason: request.goal,
        })
    }
}

struct MacroLoop;

#[async_trait]
impl LoopDriver for MacroLoop {
    fn id(&self) -> &str {
        "macro-loop"
    }

    async fn run_loop(
        &self,
        _runtime: Arc<dyn HostRuntime>,
        request: LoopRunRequest,
        _cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError> {
        Ok(DriverRunOutcome::Achieved {
            iterations: 1,
            reason: request.target,
        })
    }
}

export_native_bundle_v2!(|ctx, out| {
    if ctx.bundle_id() == "hya/reject" {
        return Err(HostError::InvalidInput("rejected".to_string()));
    }
    out.tools.push(Arc::new(MacroTool));
    out.hooks.push(Arc::new(MacroHook));
    out.services.push(Arc::new(MacroService));
    out.api_handlers.push(Arc::new(MacroApi));
    out.projection_reducers.push(Arc::new(MacroReducer));
    out.uri_schema_handlers.push(Arc::new(MacroUri));
    out.goal_drivers.push(Arc::new(MacroGoal));
    out.loop_drivers.push(Arc::new(MacroLoop));
    Ok::<(), HostError>(())
});

#[test]
fn abi_digest_v2_is_deterministic_and_distinct_from_v1() {
    let d1 = abi_digest_v2();
    let d2 = abi_digest_v2();
    assert_eq!(d1, d2);
    assert_ne!(d1, [0_u8; 32]);
    assert_ne!(d1, hya_tool::native_bundle::abi_digest_v1());

    let descriptor = NativeAbiV2::current();
    assert_eq!(descriptor.version, ABI_VERSION_V2);
    assert_eq!(descriptor.digest, d1);
}

#[test]
fn ffi_export_macro_round_trip_and_error_handling() {
    let mut actual = [0_u8; 32];
    // SAFETY: `actual` has 32 writable bytes.
    unsafe { hya_native_abi_v2(actual.as_mut_ptr()) };
    assert_eq!(actual, abi_digest_v2());

    let runtime: Arc<dyn HostRuntime> = Arc::new(InMemoryHostRuntime::default());
    let ctx = NativePluginContext::new("hya/macro", Arc::clone(&runtime));
    assert_eq!(ctx.bundle_id(), "hya/macro");
    let mut regs = NativeRegistrations::new();
    assert!(regs.is_empty());
    let mut err: Option<String> = None;
    // SAFETY: all pointers are live and match ABI v2.
    let rc = unsafe { hya_native_register_v2(&ctx, &mut regs, &mut err) };
    assert_eq!(rc, 0);
    assert!(err.is_none());
    assert!(!regs.is_empty());
    assert_eq!(regs.tools.len(), 1);
    assert_eq!(regs.hooks.len(), 1);
    assert_eq!(regs.services.len(), 1);
    assert_eq!(regs.api_handlers.len(), 1);
    assert_eq!(regs.projection_reducers.len(), 1);
    assert_eq!(regs.uri_schema_handlers.len(), 1);
    assert_eq!(regs.goal_drivers.len(), 1);
    assert_eq!(regs.loop_drivers.len(), 1);

    let mut second = NativeRegistrations::new();
    second.extend(regs);
    assert_eq!(second.tools.len(), 1);

    // Rejection path propagates diagnostic without panicking.
    let reject_ctx = NativePluginContext::new("hya/reject", runtime);
    let mut reject_regs = NativeRegistrations::new();
    let mut reject_err: Option<String> = None;
    // SAFETY: valid pointers.
    let rc = unsafe { hya_native_register_v2(&reject_ctx, &mut reject_regs, &mut reject_err) };
    assert_eq!(rc, 1);
    assert!(
        reject_err
            .as_deref()
            .is_some_and(|m| m.contains("rejected"))
    );

    // Null pointer guard returns -1 cleanly.
    let mut null_err: Option<String> = None;
    // SAFETY: testing null pointer validation.
    let rc = unsafe { hya_native_register_v2(std::ptr::null(), &mut reject_regs, &mut null_err) };
    assert_eq!(rc, -1);
    assert!(null_err.is_some());
}

#[tokio::test]
async fn host_runtime_subtraits_and_extension_traits_dispatch_cleanly()
-> Result<(), Box<dyn std::error::Error>> {
    let runtime: Arc<dyn HostRuntime> = Arc::new(InMemoryHostRuntime::default());
    let session = SessionId::new();

    // HostBundleStorage KV + CAS + Claim lifecycle
    let entry = runtime
        .bundle_storage()
        .put_state("hya/test", "global", "k1", json!({"n": 1}), None)
        .await?;
    assert_eq!(entry.version, 1);
    let conflict = runtime
        .bundle_storage()
        .put_state("hya/test", "global", "k1", json!({"n": 2}), Some(99))
        .await;
    assert!(matches!(conflict, Err(HostError::Conflict(_))));
    let lease = runtime
        .bundle_storage()
        .acquire_claim(BundleClaimRequest {
            bundle: "hya/test".to_string(),
            resource_key: "actor:main".to_string(),
            owner_id: "worker-1".to_string(),
            ttl_millis: 5_000,
            metadata: Value::Null,
        })
        .await?;
    assert_eq!(lease.epoch, 1);
    let renewed = runtime.bundle_storage().renew_claim(&lease, 5_000).await?;
    assert_eq!(renewed.epoch, 1);
    runtime.bundle_storage().release_claim(&renewed).await?;
    assert!(
        runtime
            .bundle_storage()
            .read_claim("hya/test", "actor:main")
            .await?
            .is_none()
    );

    // HostEventBus + HostTurnRunner + HostModelCaller + HostCatalogReader + HostExtensionRegistry
    let mut rx = runtime.event_bus().subscribe()?;
    runtime
        .event_bus()
        .publish_extension(session, "hya/test", "ping", json!({"ok": true}))
        .await?;
    let envelope = rx.recv().await?;
    assert_eq!(envelope.event.session(), Some(session));

    let turn = runtime
        .turn_runner()
        .run_turn(
            TurnStartRequest {
                session,
                prompt: "hello".to_string(),
                agent: None,
                model: None,
                system_override: None,
                metadata: None,
                actor_claim: None,
            },
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(turn.output_text, "ran: hello");

    let model_resp = runtime
        .model_caller()
        .complete(
            ModelCallRequest {
                model: None,
                system: None,
                messages: Vec::new(),
                tools: Vec::new(),
                temperature: None,
                max_tokens: None,
                reasoning_effort: None,
                json_schema: None,
                purpose: None,
            },
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(model_resp.text, "ok");

    assert!(runtime.extension_registry().has_bundle("hya/test").await?);
    assert_eq!(runtime.catalog_reader().list_bundles().await?.len(), 1);

    // Extension traits
    let mut state = MacroReducer.initial_state();
    MacroReducer.apply(
        &mut state,
        &ExtensionEventContext {
            session,
            seq: EventSeq(1),
            ts_millis: 1,
            bundle: "hya/macro".to_string(),
            kind: "step".to_string(),
            payload: Value::Null,
        },
    )?;
    assert_eq!(state, json!({"last_kind": "step"}));

    let uri_out = MacroUri
        .read(
            UriResolveCtx {
                session: Some(session),
                workdir: Path::new("."),
                runtime: Some(runtime.as_ref()),
            },
            "macro://item/1",
        )
        .await?;
    assert_eq!(uri_out.body, "macro://item/1");

    let _unused: BTreeMap<String, String> = BTreeMap::new();
    let _abi_err = AbiError::DigestMismatch {
        bundle: "hya/test".to_string(),
    }
    .to_string();
    Ok(())
}
