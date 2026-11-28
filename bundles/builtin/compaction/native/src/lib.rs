//! `hya/compaction`: full mode's context-reduction extension (ADR-0033,
//! ADR-0035, ADR-0039), registered over Lockstep Native ABI v2.
//!
//! Before each round, ahead of the kernel's own Rewrite, [`CompactionRewriter`]
//! spills stale tool outputs (`shake`) and then lets the route fold its own
//! window (`remote`); on `/summarize` it tries `remote`. Domain observability
//! records ([`events`]: `context_evicted`, `context_status`) are persisted as
//! [`hya_proto::Event::Extension`] under `"hya/compaction"` and folded by
//! [`CompactionProjectionReducer`], whose tool-output overrides carry the
//! eviction notices into later requests.

mod config;
pub mod events;
mod evict;
mod ladder;
mod reducer;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use hya_context::rewrite::{RewriteCtx, Rewriter, ToolError};
use hya_runtime_abi::{HostError, HostRuntime, NativePluginContext, NativeRegistrations};
use serde_json::Value;

use crate::config::{Config, KEEP_RECENT_ENV};
use crate::events::BUNDLE_COMPACTION;

pub use reducer::{CompactionProjectionReducer, CompactionReducerState, LastEvictionSummary};

/// The `shake` and `remote` steps as an ABI v2 [`Rewriter`] extension bound to
/// the host runtime.
pub struct CompactionRewriter {
    runtime: Arc<dyn HostRuntime>,
    config: OnceLock<Config>,
}

impl CompactionRewriter {
    /// Construct a rewriter bound to the host's [`HostRuntime`] (for its
    /// `config.yml` through `HostCatalogReader::bundle_config` and the
    /// fallback `HostModelCaller`).
    #[must_use]
    pub fn new(runtime: Arc<dyn HostRuntime>) -> Self {
        Self {
            runtime,
            config: OnceLock::new(),
        }
    }

    /// The bundle's settings, read on first use: its `config.yml`, then
    /// `HYA_COMPACTION_KEEP_RECENT`.
    async fn config(&self) -> Result<Config, ToolError> {
        if let Some(config) = self.config.get() {
            return Ok(*config);
        }
        let value = match self
            .runtime
            .catalog_reader()
            .bundle_config(BUNDLE_COMPACTION)
            .await
        {
            Ok(value) => value,
            Err(HostError::NotFound(_) | HostError::Unavailable(_)) => Value::Null,
            Err(error) => return Err(error.into()),
        };
        let config =
            Config::from_value(&value)?.apply_env(std::env::var(KEEP_RECENT_ENV).ok().as_deref());
        Ok(*self.config.get_or_init(|| config))
    }
}

#[async_trait]
impl Rewriter for CompactionRewriter {
    async fn before_round(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        let config = self.config().await?;
        ladder::walk(&cx, &self.runtime, config).await
    }

    async fn rewrite_now(&self, cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        ladder::compact_now(&cx, &self.runtime).await
    }
}

/// Register `hya/compaction`'s [`CompactionRewriter`] and
/// [`CompactionProjectionReducer`] into `out`.
pub fn register(ctx: &NativePluginContext, out: &mut NativeRegistrations) {
    out.rewriters
        .push(Arc::new(CompactionRewriter::new(Arc::clone(ctx.runtime()))));
    out.projection_reducers
        .push(Arc::new(CompactionProjectionReducer));
}

hya_runtime_abi::export_native_bundle_v2!(|ctx, out| {
    register(ctx, out);
});

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::sync::Mutex;

    use crate::events::{
        BUNDLE_COMPACTION, CompactionEvent, ContextEvicted, ContextStatusProjection, EvictedPart,
    };
    use async_trait::async_trait;
    use hya_bundle::{Bundle, PreparedAgent};
    use hya_context::rewrite::{Pressure, RewriteCtx, SessionLog, ToolError, Transcript};
    use hya_context::tokens::{TokenAccountingMode, TokenSource};
    use hya_proto::{
        ActorClaim, AgentName, CompactionStrategy, Envelope, Event, EventSeq, Message, MessageId,
        ModelRef, Part, PartId, Projection, SessionId, ToolCallId, ToolName, ToolPartState,
    };
    use hya_runtime_abi::{
        BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
        BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionEventContext, ExtensionKind,
        HostBundleStorage, HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry,
        HostModelCaller, HostRuntime, HostSessionStore, HostTurnRunner, ModelCallRequest,
        ModelCallResponse, ModelStreamChunk, ModelWindowInfo, NativePluginContext,
        NativeRegistrations, ProjectionReducer, SessionListFilter, SessionSummaryMeta,
        ToolOutputOverride, TurnOutcome, TurnSpawnRequest, TurnStartRequest, TurnStatus,
    };
    use serde_json::{Value, json};
    use tokio::sync::{broadcast, mpsc};
    use tokio_util::sync::CancellationToken;

    use super::*;

    struct TestHost {
        config: Value,
        compact_reply: Option<String>,
        compact_calls: Mutex<usize>,
        bus: broadcast::Sender<Envelope>,
    }

    impl TestHost {
        fn new(config: Value, compact_reply: Option<&str>) -> Arc<Self> {
            let (bus, _) = broadcast::channel(8);
            Arc::new(Self {
                config,
                compact_reply: compact_reply.map(ToString::to_string),
                compact_calls: Mutex::new(0),
                bus,
            })
        }
    }

    #[async_trait]
    impl HostSessionStore for TestHost {
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
            _session: SessionId,
            _bundle: &str,
            _kind: &str,
            _payload: Value,
            _claim: Option<ActorClaim>,
        ) -> Result<Envelope, HostError> {
            Err(HostError::Unavailable("unused".into()))
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
    impl HostBundleStorage for TestHost {
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
            _bundle: &str,
            _scope: &str,
            _key: &str,
            _value: Value,
            _expected_version: Option<u64>,
        ) -> Result<BundleStateEntry, HostError> {
            Err(HostError::Unavailable("unused".into()))
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
            _request: BundleClaimRequest,
        ) -> Result<BundleClaimLease, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }
        async fn renew_claim(
            &self,
            _lease: &BundleClaimLease,
            _ttl_millis: u64,
        ) -> Result<BundleClaimLease, HostError> {
            Err(HostError::Unavailable("unused".into()))
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
    impl HostTurnRunner for TestHost {
        async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
            Ok(SessionId::new())
        }
        async fn run_turn(
            &self,
            _request: TurnStartRequest,
            _cancel: CancellationToken,
        ) -> Result<TurnOutcome, HostError> {
            Err(HostError::Unavailable("unused".into()))
        }
        async fn spawn_session_and_run(
            &self,
            _request: TurnSpawnRequest,
            _cancel: CancellationToken,
        ) -> Result<TurnOutcome, HostError> {
            Err(HostError::Unavailable("unused".into()))
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
    impl HostModelCaller for TestHost {
        async fn complete(
            &self,
            _request: ModelCallRequest,
            _cancel: CancellationToken,
        ) -> Result<ModelCallResponse, HostError> {
            Err(HostError::Unavailable("unused".into()))
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
            *self.compact_calls.lock().unwrap() += 1;
            Ok(self.compact_reply.clone())
        }
        async fn model_window(
            &self,
            _model: &ModelRef,
        ) -> Result<Option<ModelWindowInfo>, HostError> {
            Ok(None)
        }
    }

    #[async_trait]
    impl HostEventBus for TestHost {
        fn publish(&self, envelope: Envelope) -> Result<(), HostError> {
            let _ = self.bus.send(envelope);
            Ok(())
        }
        fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
            Ok(self.bus.subscribe())
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
    impl HostCatalogReader for TestHost {
        async fn list_agents(
            &self,
            _session: Option<SessionId>,
        ) -> Result<Vec<hya_tool::AgentDef>, HostError> {
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
            Ok(self.config.clone())
        }
        async fn global_config(&self) -> Result<Value, HostError> {
            Ok(json!({ "schema_version": 2 }))
        }
    }

    #[async_trait]
    impl HostExtensionRegistry for TestHost {
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

    impl HostRuntime for TestHost {
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

    struct RecordingLog {
        events: Mutex<Vec<Event>>,
    }

    impl RecordingLog {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl SessionLog for RecordingLog {
        async fn projection(&self, _session: SessionId) -> Result<Arc<Projection>, ToolError> {
            Ok(Arc::new(Projection::default()))
        }
        async fn append(&self, _session: SessionId, events: Vec<Event>) -> Result<(), ToolError> {
            self.events.lock().unwrap().extend(events);
            Ok(())
        }
    }

    struct StaticTranscript(Vec<Message>);

    #[async_trait]
    impl Transcript for StaticTranscript {
        async fn messages(&self, _projection: &Projection) -> Result<Vec<Message>, ToolError> {
            Ok(self.0.clone())
        }
    }

    fn assistant_tool_msg(part_id: PartId, output: &str) -> Message {
        Message::Assistant {
            id: MessageId::new(),
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("test/model"),
            parts: vec![Part::Tool {
                id: part_id,
                call_id: ToolCallId::new(),
                name: ToolName::new("grep"),
                state: ToolPartState::Completed {
                    input: json!({"pattern": "foo"}),
                    output: Value::String(output.to_string()),
                    time_ms: 5,
                },
                provider_data: None,
            }],
            finish: None,
            tokens: None,
            last_round: None,
            last_step_start: None,
        }
    }

    #[tokio::test]
    async fn abi_v2_registration_rewriter_host_runtime_and_projection_reducer() {
        let host = TestHost::new(
            json!({ "compaction": { "keep_recent": 1 } }),
            Some("REMOTE_COMPACT_MARKER"),
        );
        let runtime: Arc<dyn HostRuntime> = host.clone();
        let ctx = NativePluginContext::new(BUNDLE_COMPACTION, Arc::clone(&runtime));
        let mut regs = NativeRegistrations::new();
        register(&ctx, &mut regs);

        assert!(regs.tools.is_empty());
        assert_eq!(regs.rewriters.len(), 1);
        assert_eq!(regs.projection_reducers.len(), 1);
        assert_eq!(regs.projection_reducers[0].bundle_id(), BUNDLE_COMPACTION);

        let session = SessionId::new();
        let old_part = PartId::new();
        let recent_part = PartId::new();
        let messages = vec![
            assistant_tool_msg(old_part, &"STALE_BODY_".repeat(120)),
            assistant_tool_msg(recent_part, "fresh"),
        ];
        let dir = std::env::temp_dir().join(format!("hya-compact-v2-{}", SessionId::new()));
        std::fs::create_dir_all(&dir).unwrap();

        let log = RecordingLog::new();
        let transcript = StaticTranscript(messages);
        let projection = Projection::default();
        let model = ModelRef::new("test/model");

        // `keep_recent` comes from the host's `bundle_config`; `models: None`
        // exercises the `HostModelCaller` fallback.
        let changed = regs.rewriters[0]
            .before_round(RewriteCtx {
                session,
                message: MessageId::new(),
                model: &model,
                projection: &projection,
                window: None,
                overhead_tokens: 0,
                pressure: Some(Pressure {
                    used: 9_000,
                    threshold: 1_000,
                }),
                workdir: &dir,
                token_accounting: TokenAccountingMode::Estimate,
                log: &log,
                transcript: &transcript,
                models: None,
                hooks: None,
            })
            .await
            .unwrap();
        assert!(changed);
        assert_eq!(*host.compact_calls.lock().unwrap(), 1);

        let recorded = log.events.lock().unwrap().clone();
        let mut ext_state = regs.projection_reducers[0].initial_state();
        let mut saw_evicted = false;
        let mut saw_status = false;
        let mut saw_native_compact = false;
        for (idx, event) in recorded.iter().enumerate() {
            match event {
                Event::Extension {
                    session: ev_session,
                    bundle,
                    kind,
                    payload,
                } => {
                    assert_eq!(*ev_session, session);
                    assert_eq!(bundle, BUNDLE_COMPACTION);
                    if kind == "context_evicted" {
                        saw_evicted = true;
                    }
                    if kind == "context_status" {
                        saw_status = true;
                    }
                    regs.projection_reducers[0]
                        .apply(
                            &mut ext_state,
                            &ExtensionEventContext {
                                session: *ev_session,
                                seq: EventSeq((idx + 1) as u64),
                                ts_millis: 1_700_000_000_000,
                                bundle: bundle.clone(),
                                kind: kind.clone(),
                                payload: payload.clone(),
                            },
                        )
                        .unwrap();
                }
                Event::ContextCompacted { strategy, .. }
                    if *strategy == CompactionStrategy::Native =>
                {
                    saw_native_compact = true;
                }
                _ => {}
            }
        }
        assert!(saw_evicted, "expected context_evicted Event::Extension");
        assert!(saw_status, "expected context_status Event::Extension");
        assert!(saw_native_compact, "expected native ContextCompacted event");

        let folded: CompactionReducerState = serde_json::from_value(ext_state).unwrap();
        assert!(folded.context_status.is_some());
        assert_eq!(folded.context_status.unwrap().threshold, 1_000);
        assert_eq!(folded.last_eviction.unwrap().evicted_parts, 1);
        assert!(
            folded
                .evicted_parts
                .get(&old_part.to_string())
                .is_some_and(|notice| notice.contains("artifact://"))
        );

        // Exercise `rewrite_now` via HostModelCaller fallback.
        let log2 = RecordingLog::new();
        let changed_now = regs.rewriters[0]
            .rewrite_now(RewriteCtx {
                session,
                message: MessageId::new(),
                model: &model,
                projection: &projection,
                window: None,
                overhead_tokens: 0,
                pressure: None,
                workdir: &dir,
                token_accounting: TokenAccountingMode::Estimate,
                log: &log2,
                transcript: &transcript,
                models: None,
                hooks: None,
            })
            .await
            .unwrap();
        assert!(changed_now);
        assert_eq!(*host.compact_calls.lock().unwrap(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn projection_reducer_is_deterministic_and_ignores_foreign_bundles() {
        let reducer = CompactionProjectionReducer;
        let session = SessionId::new();
        let part = PartId::new();
        let mut state = reducer.initial_state();

        // Foreign bundle event is ignored.
        reducer
            .apply(
                &mut state,
                &ExtensionEventContext {
                    session,
                    seq: EventSeq(1),
                    ts_millis: 1,
                    bundle: "hya/other".to_string(),
                    kind: "context_status".to_string(),
                    payload: json!({"tokens": 999}),
                },
            )
            .unwrap();
        assert_eq!(state, json!({}));

        let Event::Extension {
            bundle,
            kind,
            payload,
            ..
        } = CompactionEvent::ContextEvicted(ContextEvicted {
            evicted_parts: 1,
            parts: vec![EvictedPart {
                part,
                notice: "[tool output moved to artifact://abc]".to_string(),
            }],
            tokens_before: 5000,
            tokens_after: 1200,
            threshold: 4000,
        })
        .into_event(session)
        else {
            panic!("expected extension event");
        };
        let ev_ctx = ExtensionEventContext {
            session,
            seq: EventSeq(2),
            ts_millis: 2,
            bundle,
            kind,
            payload,
        };
        reducer.apply(&mut state, &ev_ctx).unwrap();
        let after_first = state.clone();
        // Applying the same event again is idempotent.
        reducer.apply(&mut state, &ev_ctx).unwrap();
        assert_eq!(state, after_first);
        // The notice replaces the part's output in later requests.
        assert_eq!(
            reducer.tool_output_overrides(&ev_ctx),
            vec![ToolOutputOverride {
                part,
                output: "[tool output moved to artifact://abc]".to_string(),
            }]
        );

        let Event::Extension {
            bundle: s_bundle,
            kind: s_kind,
            payload: s_payload,
            ..
        } = CompactionEvent::ContextStatus(ContextStatusProjection {
            tokens: 1200,
            source: TokenSource::Estimate,
            mode: TokenAccountingMode::Auto,
            threshold: 4000,
        })
        .into_event(session)
        else {
            panic!("expected extension event");
        };
        reducer
            .apply(
                &mut state,
                &ExtensionEventContext {
                    session,
                    seq: EventSeq(3),
                    ts_millis: 3,
                    bundle: s_bundle,
                    kind: s_kind,
                    payload: s_payload.clone(),
                },
            )
            .unwrap();
        let status_ctx = ExtensionEventContext {
            session,
            seq: EventSeq(3),
            ts_millis: 3,
            bundle: BUNDLE_COMPACTION.to_string(),
            kind: "context_status".to_string(),
            payload: s_payload,
        };
        assert!(reducer.tool_output_overrides(&status_ctx).is_empty());

        let decoded: CompactionReducerState = serde_json::from_value(state).unwrap();
        assert_eq!(
            decoded
                .evicted_parts
                .get(&part.to_string())
                .map(String::as_str),
            Some("[tool output moved to artifact://abc]")
        );
        assert_eq!(decoded.context_status.unwrap().tokens, 1200);
        assert_eq!(decoded.last_eviction.unwrap().tokens_after, 1200);
    }
}
