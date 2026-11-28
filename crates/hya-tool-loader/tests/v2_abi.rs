//! Integration tests for Native ABI v2 cdylib loading, runtime bridging, and
//! non-panicking selective loading in `hya-tool-loader`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_bundle::{Bundle, BundleIdentity, PreparedAgent};
use hya_context::rewrite::{CompactionHooks, ModelHost, RewriteCtx, SessionLog, Transcript};
use hya_proto::{
    ActorClaim, AgentName, Envelope, Event, EventSeq, FinishReason, Message, MessageId, ModelRef,
    Projection, SessionId, ToolCallId, ToolSchema,
};
use hya_runtime_abi::{
    AbiError, BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
    BundleClaimRequest, BundleStateEntry, DriverRequest, ExtensionEntry, ExtensionEventContext,
    ExtensionKind, HostBundleStorage, HostCatalogReader, HostError, HostEventBus,
    HostExtensionRegistry, HostModelCaller, HostRuntime, HostSessionStore, HostTurnRunner,
    ModelCallRequest, ModelCallResponse, ModelStreamChunk, ModelWindowInfo, NativeBundleApiRequest,
    NativeCompactionBeforeInput, NativeCompactionDecision, NativeCompactionTrigger,
    NativeToolBeforeInput, NativeToolBeforeOutcome, SessionListFilter, SessionSummaryMeta,
    ToolVisibilityInput, ToolVisibilityOutcome, TurnOutcome, TurnSpawnRequest, TurnStartRequest,
    TurnStatus, UriResolveCtx,
};
use hya_tool::{AgentDef, PermissionPlane, PermissionRules, SkillCatalogEntry, ToolCtx};
use hya_tool_loader::{NativeBundleLoader, load_v2_from_path, try_from_tool_families};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct MockHostRuntime {
    states: Mutex<HashMap<(String, String, String), BundleStateEntry>>,
    bus: MockBus,
}

struct MockBus {
    tx: broadcast::Sender<Envelope>,
}

impl Default for MockBus {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self { tx }
    }
}

#[async_trait]
impl HostSessionStore for MockHostRuntime {
    async fn session_exists(&self, _session: SessionId) -> Result<bool, HostError> {
        Ok(true)
    }

    async fn read_events(
        &self,
        _session: SessionId,
        _after_seq: Option<EventSeq>,
        _limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError> {
        Ok(Vec::new())
    }

    async fn append_events(
        &self,
        _session: SessionId,
        _claim: Option<ActorClaim>,
        _events: Vec<Event>,
    ) -> Result<Vec<Envelope>, HostError> {
        Ok(Vec::new())
    }

    async fn append_extension_event(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
        _claim: Option<ActorClaim>,
    ) -> Result<Envelope, HostError> {
        Ok(Envelope {
            seq: EventSeq(1),
            ts_millis: 1,
            event: Event::SessionMetadataSet {
                session,
                metadata: json!({ "bundle": bundle, "kind": kind, "payload": payload }),
            },
        })
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
        _session: SessionId,
    ) -> Result<Option<SessionSummaryMeta>, HostError> {
        Ok(None)
    }

    async fn list_sessions(
        &self,
        _filter: SessionListFilter,
    ) -> Result<Vec<SessionSummaryMeta>, HostError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl HostBundleStorage for MockHostRuntime {
    async fn get_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
    ) -> Result<Option<BundleStateEntry>, HostError> {
        let map = self
            .states
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        Ok(map
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
        let mut map = self
            .states
            .lock()
            .map_err(|e| HostError::Internal(e.to_string()))?;
        let map_key = (bundle.to_string(), scope.to_string(), key.to_string());
        let next_ver = map.get(&map_key).map_or(1, |e| e.version + 1);
        let entry = BundleStateEntry {
            bundle: bundle.to_string(),
            scope: scope.to_string(),
            key: key.to_string(),
            value,
            version: next_ver,
            updated_at_millis: 1_700_000_000_000,
        };
        map.insert(map_key, entry.clone());
        Ok(entry)
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
        Ok(BundleClaimLease {
            bundle: request.bundle,
            resource_key: request.resource_key,
            owner_id: request.owner_id,
            epoch: 1,
            expires_at_millis: 1000,
            metadata: request.metadata,
        })
    }

    async fn renew_claim(
        &self,
        lease: &BundleClaimLease,
        _ttl_millis: u64,
    ) -> Result<BundleClaimLease, HostError> {
        Ok(lease.clone())
    }

    async fn release_claim(&self, _lease: &BundleClaimLease) -> Result<(), HostError> {
        Ok(())
    }

    async fn read_claim(
        &self,
        _bundle: &str,
        _resource_key: &str,
    ) -> Result<Option<BundleClaimLease>, HostError> {
        Ok(None)
    }
}

#[async_trait]
impl HostTurnRunner for MockHostRuntime {
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
            output_text: format!("goal-turn: {}", request.prompt.unwrap_or_default()),
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
            output_text: request.prompt,
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
}

#[async_trait]
impl HostModelCaller for MockHostRuntime {
    async fn complete(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelCallResponse, HostError> {
        Ok(ModelCallResponse {
            model: ModelRef::new("dev/echo"),
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
        let (_tx, rx) = mpsc::channel(1);
        Ok(rx)
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
impl HostEventBus for MockHostRuntime {
    fn publish(&self, envelope: Envelope) -> Result<(), HostError> {
        let _ = self.bus.tx.send(envelope);
        Ok(())
    }

    fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
        Ok(self.bus.tx.subscribe())
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
impl HostCatalogReader for MockHostRuntime {
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
                id: "hya/probe".to_string(),
                version: "0.51.0".to_string(),
                publisher: "hya".to_string(),
            },
            enabled: true,
            description: None,
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
impl HostExtensionRegistry for MockHostRuntime {
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
        Ok(BundleApiResult {
            status: 200,
            body: Value::Null,
        })
    }

    async fn invoke_tool(
        &self,
        _session: Option<SessionId>,
        _tool_name: &str,
        input: Value,
        _cancel: CancellationToken,
    ) -> Result<Value, HostError> {
        Ok(input)
    }
}

impl HostRuntime for MockHostRuntime {
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

struct DummySessionLog;
#[async_trait]
impl SessionLog for DummySessionLog {
    async fn projection(
        &self,
        _session: SessionId,
    ) -> Result<Arc<Projection>, hya_tool_error::ToolError> {
        Ok(Arc::new(Projection::default()))
    }

    async fn append(
        &self,
        _session: SessionId,
        _events: Vec<Event>,
    ) -> Result<(), hya_tool_error::ToolError> {
        Ok(())
    }
}

struct DummyTranscript;
#[async_trait]
impl Transcript for DummyTranscript {
    async fn messages(
        &self,
        _projection: &Projection,
    ) -> Result<Vec<Message>, hya_tool_error::ToolError> {
        Ok(Vec::new())
    }
}

struct DummyModelHost;
#[async_trait]
impl ModelHost for DummyModelHost {
    async fn compact_window(
        &self,
        _model: &ModelRef,
        _messages: &[Message],
    ) -> Result<Option<String>, hya_tool_error::ToolError> {
        Ok(None)
    }
}

struct DummyCompactionHooks;
#[async_trait]
impl CompactionHooks for DummyCompactionHooks {
    async fn before(&self, _session: SessionId, _tokens: usize) {}
    async fn after(&self, _session: SessionId, _summary_tokens: usize) {}
}

struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn find_example_cdylib(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let examples = exe
        .ancestors()
        .map(|dir| dir.join("examples"))
        .find(|dir| dir.is_dir())
        .ok_or("cargo test did not build cdylib examples directory")?;
    let filename = format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        name,
        std::env::consts::DLL_SUFFIX
    );
    let path = examples.join(filename);
    if !path.is_file() {
        return Err(format!("missing cdylib fixture at {}", path.display()).into());
    }
    Ok(path)
}

fn test_tool_ctx(session: SessionId) -> ToolCtx {
    let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (permission, _prx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
    let mut ctx = ToolCtx::new(workdir, permission, CancellationToken::new());
    ctx.session = Some(session);
    ctx
}

#[cfg(unix)]
#[tokio::test]
async fn cdylib_v2_registration_runtime_bridge_and_host_dispatch()
-> Result<(), Box<dyn std::error::Error>> {
    let lib_path = find_example_cdylib("test_v2_bundle")?;
    let runtime: Arc<dyn HostRuntime> = Arc::new(MockHostRuntime::default());

    let regs = load_v2_from_path("hya/probe", &lib_path, Arc::clone(&runtime))?;
    assert_eq!(regs.tools.len(), 1);
    assert_eq!(regs.rewriters.len(), 1);
    assert_eq!(regs.services.len(), 1);
    assert_eq!(regs.hooks.len(), 1);
    assert_eq!(regs.api_handlers.len(), 1);
    assert_eq!(regs.projection_reducers.len(), 1);
    assert_eq!(regs.uri_schema_handlers.len(), 1);
    assert_eq!(regs.drivers.len(), 1);

    let session = SessionId::new();
    let parent = SessionId::new();
    let ctx = test_tool_ctx(session).with_extension(hya_tool::ParentSession(parent));

    // Tool execution + scoped deadline cell + Tokio sleep inside cdylib + HostRuntime KV write + ExtensionPlanes across cdylib boundary
    let _deadline = hya_tool::deadline::install_scoped(std::time::Duration::from_secs(42));
    let tool_out = regs.tools[0].execute(&ctx, json!({"hello": "v2"})).await?;
    assert_eq!(tool_out["bundle_id"], "hya/probe");
    assert_eq!(tool_out["written_version"], 1);
    assert_eq!(tool_out["read_value"], json!({"hello": "v2"}));
    assert_eq!(tool_out["budget_secs"], 42);
    assert_eq!(tool_out["parent_session"], parent.to_string());

    // Rewriter execution across runtime bridge
    let projection = Projection::default();
    let model = ModelRef::new("dev/echo");
    let rewrite_ctx = RewriteCtx {
        session,
        message: MessageId::new(),
        model: &model,
        projection: &projection,
        window: None,
        overhead_tokens: 0,
        pressure: None,
        workdir: &ctx.workdir,
        token_accounting: hya_context::tokens::TokenAccountingMode::Auto,
        log: &DummySessionLog,
        transcript: &DummyTranscript,
        models: Some(&DummyModelHost),
        hooks: Some(&DummyCompactionHooks),
    };
    assert!(!regs.rewriters[0].before_round(rewrite_ctx).await?);
    assert!(regs.rewriters[0].rewrite_now(rewrite_ctx).await?);

    // NativeService lifecycle
    assert!(!regs.services[0].health().await.healthy);
    regs.services[0].start(Arc::clone(&runtime)).await?;
    assert!(regs.services[0].health().await.healthy);
    regs.services[0].stop().await?;
    assert!(!regs.services[0].health().await.healthy);

    // NativeHook dispatch
    let veto = regs.hooks[0]
        .tool_execute_before(NativeToolBeforeInput {
            session,
            message: MessageId::new(),
            call: ToolCallId::new(),
            tool: "blocked_tool".to_string(),
            input: json!({}),
        })
        .await?;
    assert!(matches!(veto, NativeToolBeforeOutcome::Veto { .. }));

    let vis = regs.hooks[0]
        .tool_visibility(ToolVisibilityInput {
            session,
            agent: None,
            candidate_tools: vec!["read".to_string(), "edit".to_string()],
            interactive: true,
            depth: 0,
        })
        .await?;
    assert_eq!(
        vis,
        ToolVisibilityOutcome::Hide {
            tools: vec!["edit".to_string()]
        }
    );

    let comp = regs.hooks[0]
        .compaction_before(NativeCompactionBeforeInput {
            session,
            trigger: NativeCompactionTrigger::Proactive,
            messages_token_estimate: 1000,
        })
        .await?;
    assert!(matches!(comp, NativeCompactionDecision::Replace { .. }));

    // Every CHR-43 lifecycle point crosses the cdylib boundary to the bundle's
    // implementation, not the trait default.
    let hook = &regs.hooks[0];
    let permit = hook
        .model_stream_permit(hya_runtime_abi::NativeStreamPermitInput {
            session,
            root: parent,
            depth: 2,
        })
        .await?;
    assert!(permit.is_some());
    hook.turn_stop(hya_runtime_abi::NativeTurnStopInput {
        session,
        cause: hya_proto::FinishCause::UserCancel,
    })
    .await?;
    assert!(
        hook.turn_stop(hya_runtime_abi::NativeTurnStopInput {
            session,
            cause: hya_proto::FinishCause::Archived,
        })
        .await
        .is_err()
    );
    assert!(matches!(
        hook.session_close(hya_runtime_abi::NativeSessionCloseInput {
            session,
            kind: hya_runtime_abi::SessionCloseKind::Delete,
        })
        .await,
        Err(HostError::PermissionDenied(_))
    ));
    hook.session_close(hya_runtime_abi::NativeSessionCloseInput {
        session,
        kind: hya_runtime_abi::SessionCloseKind::Archive,
    })
    .await?;
    assert_eq!(
        hook.headless_run(hya_runtime_abi::NativeHeadlessRunInput {
            session,
            phase: hya_runtime_abi::HeadlessRunPhase::Start,
        })
        .await?
        .as_deref(),
        Some("probe Start")
    );
    assert_eq!(
        hook.session_restore(hya_runtime_abi::NativeSessionRestoreInput { session })
            .await?,
        Some(format!("restore {session}"))
    );
    assert_eq!(
        hook.runtime_restored().await,
        Err(HostError::Unavailable("probe restored".into()))
    );
    assert_eq!(
        regs.projection_reducers[0].observed_bundles(),
        ["hya/observed"]
    );
    assert!(regs.tools[0].ends_turn());
    assert!(regs.tools[0].suppresses_notices());

    // NativeBundleApiHandler
    let api_reply = regs.api_handlers[0]
        .handle(NativeBundleApiRequest {
            api: "ping".to_string(),
            method: hya_bundle::ApiMethod::Post,
            scope: hya_bundle::ApiScope::Global,
            path: "/ping".to_string(),
            path_params: BTreeMap::new(),
            query: BTreeMap::new(),
            body: json!({"n": 7}),
            session: None,
        })
        .await?;
    assert_eq!(api_reply.status, 200);
    assert_eq!(api_reply.body["echo"], json!({"n": 7}));

    // ProjectionReducer
    let mut ext_state = regs.projection_reducers[0].initial_state();
    regs.projection_reducers[0].apply(
        &mut ext_state,
        &ExtensionEventContext {
            session,
            seq: EventSeq(1),
            ts_millis: 10,
            bundle: "hya/probe".to_string(),
            kind: "started".to_string(),
            payload: Value::Null,
        },
    )?;
    assert_eq!(ext_state, json!({"count": 1, "kinds": ["started"]}));

    // UriSchemaHandler
    let uri_ctx = UriResolveCtx {
        session: Some(session),
        actor_claim: None,
        workdir: &ctx.workdir,
        runtime: Some(runtime.as_ref()),
    };
    let read_res = regs.uri_schema_handlers[0]
        .read(uri_ctx, "channel://main")
        .await?;
    assert_eq!(read_res.body, "resolved:channel://main:runtime=true");

    let write_res = regs.uri_schema_handlers[0]
        .write(
            UriResolveCtx {
                session: Some(session),
                actor_claim: None,
                workdir: &ctx.workdir,
                runtime: Some(runtime.as_ref()),
            },
            "channel://main",
            "hello",
        )
        .await?;
    assert_eq!(write_res["bytes"], 5);

    // NativeDriver
    let report = regs.drivers[0]
        .run(
            Arc::clone(&runtime),
            DriverRequest {
                session,
                agent: None,
                model: None,
                workdir: std::env::temp_dir(),
                args: serde_json::json!({ "prompt": "ship v2" }),
            },
            CancellationToken::new(),
        )
        .await?;
    assert_eq!(report.stdout, "goal-turn: ship v2");
    assert_eq!(report.exit_code, 0);

    // Verify registration error propagation from cdylib without panicking.
    let err = load_v2_from_path("hya/fail-register", &lib_path, runtime)
        .err()
        .ok_or("expected RegistrationFailed error")?;
    assert!(matches!(err, AbiError::RegistrationFailed { .. }));

    // Ensure ToolSchema is valid
    let _schema: ToolSchema = regs.tools[0].schema();
    Ok(())
}

#[cfg(unix)]
#[test]
fn cdylib_v2_digest_mismatch_is_rejected_without_calling_register()
-> Result<(), Box<dyn std::error::Error>> {
    let wrong_path = find_example_cdylib("wrong_abi_v2_bundle")?;
    let runtime: Arc<dyn HostRuntime> = Arc::new(MockHostRuntime::default());
    let err = load_v2_from_path("hya/wrong", &wrong_path, runtime)
        .err()
        .ok_or("expected DigestMismatch error")?;
    assert_eq!(
        err,
        AbiError::DigestMismatch {
            bundle: "hya/wrong".to_string()
        }
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn selective_loader_skips_disabled_and_errors_cleanly_on_missing()
-> Result<(), Box<dyn std::error::Error>> {
    let lib_path = find_example_cdylib("test_v2_bundle")?;
    let temp_dir = std::env::temp_dir().join(format!(
        "hya-tool-loader-selective-v2-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&temp_dir)?;
    let _guard = TempDirGuard(temp_dir.clone());

    let mapped_name = format!(
        "{}hya_probe_v2{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    std::fs::copy(&lib_path, temp_dir.join(mapped_name))?;

    let runtime: Arc<dyn HostRuntime> = Arc::new(MockHostRuntime::default());
    let loader = NativeBundleLoader::new(runtime)
        .with_disabled(["hya/disabled-bundle"])
        .with_search_dirs(vec![temp_dir]);

    let loaded = loader.load_selected(&["hya/disabled-bundle", "hya/probe-v2"])?;
    assert_eq!(loaded.skipped_disabled, vec!["hya/disabled-bundle"]);
    assert_eq!(loaded.loaded_identities, vec!["hya/probe-v2"]);
    assert_eq!(loaded.registrations.tools.len(), 1);

    let missing_err = loader
        .load_selected(&["hya/missing-bundle"])
        .err()
        .ok_or("expected DisabledOrMissing error for absent bundle")?;
    assert!(matches!(missing_err, AbiError::DisabledOrMissing { .. }));

    // An id that is no first-party native bundle fails to load, cleanly.
    let v1_err = try_from_tool_families(&["hya/unknown-family"])
        .err()
        .ok_or("expected a load error for an unknown bundle")?;
    assert!(v1_err.contains("load hya/unknown-family"), "{v1_err}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn missing_native_bundle_library_fails_startup_load() -> Result<(), Box<dyn std::error::Error>> {
    const CHILD: &str = "HYA_TEST_MISSING_NATIVE_LIBRARY_CHILD";
    if std::env::var_os(CHILD).is_some() {
        // The host's startup load path (`hya-app` runtime) for an enabled bundle.
        let Err(error) = hya_tool_loader::load_native_bundles(
            &["hya/compaction"],
            Arc::new(hya_tool_loader::MinimalHostRuntime::empty()),
        ) else {
            panic!("startup must return a bundle load error");
        };
        assert!(error.contains("hya/compaction"), "{error}");
        return Ok(());
    }

    // A fresh process with no adjacent libraries cannot reuse the loader cache.
    let exe = std::env::current_exe()?;
    let dir = exe.with_file_name(format!("missing-native-library-{}", SessionId::new()));
    std::fs::create_dir(&dir)?;
    let child = dir.join("fixture");
    let result = std::fs::hard_link(exe, &child).and_then(|()| {
        std::process::Command::new(child)
            .args([
                "--exact",
                "missing_native_bundle_library_fails_startup_load",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
    });
    std::fs::remove_dir_all(dir)?;
    let output = result?;
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "{output:?}"
    );
    Ok(())
}
