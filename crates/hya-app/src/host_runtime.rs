//! App-layer [`hya_runtime_abi::HostRuntime`] implementation injected into
//! ABI v2 native bundles (`hya/coding-extras`, `hya/hashline`, `hya/goal`, etc.).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use futures::StreamExt as _;
use hya_bundle::{Bundle, PreparedAgent};
use hya_engine::hooks::{GoalEvaluateReply, HookDispatcher};
use hya_engine::{AgentSpec, CoreError, HookChain, SessionEngine, TurnBinding};
use hya_proto::{
    ActorClaim, AgentName, Envelope, Event, EventSeq, FinishReason, Message, MessageId, ModelRef,
    PartProjection, Projection, Role, SessionId, TokenUsage,
};
use hya_provider::{CompletionRequest, ProviderRouter, ReasoningEffort};
use hya_runtime_abi::{
    BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
    BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionKind, GoalDriver,
    HostBundleStorage, HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry,
    HostModelCaller, HostRuntime, HostSessionStore, HostTurnRunner, ModelCallRequest,
    ModelCallResponse, ModelStreamChunk, ModelToolCall, ModelWindowInfo, NativeGoalEvaluateVerdict,
    SessionListFilter, SessionSummaryMeta, TurnOutcome, TurnSpawnRequest, TurnStartRequest,
    TurnStatus,
};
use hya_store::SessionStore;
use hya_tool::{AgentDef, SkillCatalogEntry};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

/// [`HostRuntime`] backed by `hya-app`'s [`SessionStore`], [`ProviderRouter`],
/// active `config.yaml` path, and late-attached [`SessionEngine`] + native drivers.
pub(crate) struct AppHostRuntime {
    store: SessionStore,
    config_file: PathBuf,
    router: Arc<ProviderRouter>,
    default_agent: AgentSpec,
    engine: OnceLock<Weak<SessionEngine>>,
    goal_driver: OnceLock<Arc<dyn GoalDriver>>,
}

impl AppHostRuntime {
    /// Construct a host runtime for native bundles.
    pub(crate) fn new(
        store: SessionStore,
        config_file: PathBuf,
        router: Arc<ProviderRouter>,
        default_agent: AgentSpec,
    ) -> Self {
        Self {
            store,
            config_file,
            router,
            default_agent,
            engine: OnceLock::new(),
            goal_driver: OnceLock::new(),
        }
    }

    /// Attach the constructed [`SessionEngine`] via a weak reference (avoiding
    /// an `Arc` cycle while enabling turn execution, hooks, catalog, and bus).
    pub(crate) fn attach_engine(&self, engine: &Arc<SessionEngine>) {
        let _ = self.engine.set(Arc::downgrade(engine));
    }

    /// Register the active native [`GoalDriver`] loaded from `hya/goal`.
    pub(crate) fn attach_goal_driver(&self, driver: Arc<dyn GoalDriver>) {
        let _ = self.goal_driver.set(driver);
    }

    fn engine(&self) -> Result<Arc<SessionEngine>, HostError> {
        self.engine
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| HostError::Unavailable("session engine is not attached".into()))
    }

    async fn resolve_session_scope(
        &self,
        session: Option<SessionId>,
    ) -> Result<(PathBuf, String), HostError> {
        if let Some(session_id) = session {
            let projection = self.read_projection(session_id).await?;
            let workdir = projection
                .session
                .workdir
                .as_deref()
                .filter(|w| !w.is_empty())
                .map_or_else(|| self.default_agent.workdir.clone(), PathBuf::from);
            let agent = projection.session.agent.as_ref().map_or_else(
                || self.default_agent.name.as_str().to_string(),
                |a| a.as_str().to_string(),
            );
            return Ok((workdir, agent));
        }
        Ok((
            self.default_agent.workdir.clone(),
            self.default_agent.name.as_str().to_string(),
        ))
    }

    async fn bind_for_workdir(&self, workdir: &Path) -> Result<TurnBinding, HostError> {
        let engine = self.engine()?;
        engine
            .bind_root_runtime(workdir)
            .await
            .map_err(map_core_error)
    }
}

fn map_core_error(error: CoreError) -> HostError {
    match error {
        CoreError::Cancelled => HostError::Cancelled,
        CoreError::Invalid(msg) => HostError::InvalidInput(msg),
        CoreError::Store(err) => HostError::Storage(err.to_string()),
        CoreError::CapacityExceeded { .. } | CoreError::TurnAlreadyActive { .. } => {
            HostError::Unavailable(error.to_string())
        }
        other => HostError::Internal(other.to_string()),
    }
}

fn goal_evaluator_hook_chain(
    binding: &TurnBinding,
    stable_agent_id: &str,
) -> Arc<dyn HookDispatcher> {
    let is_goal_owner = |id: &str| id == "hya/goal" || id == "hya/goal-loop";
    let active_is_goal_owner = binding
        .resolve_agent(stable_agent_id)
        .is_some_and(|agent| agent.origin.bundle_id().is_some_and(is_goal_owner));
    let selected = binding.bundle_hooks_for_agent(if active_is_goal_owner {
        hya_engine::MAIN_AGENT_ID
    } else {
        stable_agent_id
    });
    let agentless = binding
        .bundle_catalog()
        .bundles()
        .iter()
        .filter(|bundle| {
            (bundle.agents().is_empty() && bundle.workflow().is_none())
                && !is_goal_owner(&bundle.identity().id)
        })
        .filter_map(|bundle| binding.bundle_hooks(&bundle.identity().id))
        .collect::<Vec<_>>();
    let mut hooks = Vec::new();
    for hook in binding
        .global_plugin_hooks()
        .into_iter()
        .chain(agentless)
        .chain(selected)
        .chain(binding.bundle_hooks("hya/goal"))
        .chain(binding.bundle_hooks("hya/goal-loop"))
    {
        if !hooks.iter().any(|existing| Arc::ptr_eq(existing, &hook)) {
            hooks.push(hook);
        }
    }
    Arc::new(HookChain::new(hooks))
}

#[async_trait]
impl HostSessionStore for AppHostRuntime {
    async fn session_exists(&self, session: SessionId) -> Result<bool, HostError> {
        self.store
            .read_projection_shared(session)
            .await
            .map(|projection| projection.last_seq > 0)
            .map_err(|error| HostError::Storage(error.to_string()))
    }

    async fn read_events(
        &self,
        session: SessionId,
        after_seq: Option<EventSeq>,
        limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError> {
        let mut envelopes = self
            .store
            .replay(session)
            .await
            .map_err(|error| HostError::Storage(error.to_string()))?;
        if let Some(seq) = after_seq {
            envelopes.retain(|env| env.seq > seq);
        }
        if let Some(max) = limit {
            envelopes.truncate(max);
        }
        Ok(envelopes)
    }

    async fn append_events(
        &self,
        session: SessionId,
        _claim: Option<ActorClaim>,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, HostError> {
        let mut appended = Vec::with_capacity(events.len());
        for event in events {
            let (seq, ts_millis) = self
                .store
                .append_event(session, &event)
                .await
                .map_err(|error| HostError::Storage(error.to_string()))?;
            let env = Envelope {
                seq,
                ts_millis,
                event,
            };
            if let Ok(engine) = self.engine() {
                engine.bus().publish(env.clone());
            }
            appended.push(env);
        }
        Ok(appended)
    }

    async fn append_extension_event(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
        _claim: Option<ActorClaim>,
    ) -> Result<Envelope, HostError> {
        let event = Event::Extension {
            session,
            bundle: bundle.to_string(),
            kind: kind.to_string(),
            payload,
        };
        let (seq, ts_millis) = self
            .store
            .append_event(session, &event)
            .await
            .map_err(|error| HostError::Storage(error.to_string()))?;
        let env = Envelope {
            seq,
            ts_millis,
            event,
        };
        if let Ok(engine) = self.engine() {
            engine.bus().publish(env.clone());
        }
        Ok(env)
    }

    async fn read_projection(&self, session: SessionId) -> Result<Arc<Projection>, HostError> {
        self.store
            .read_projection_shared(session)
            .await
            .map_err(|error| HostError::Storage(error.to_string()))
    }

    async fn read_extension_state(
        &self,
        session: SessionId,
        bundle: &str,
    ) -> Result<Option<Value>, HostError> {
        let projection = self.read_projection(session).await?;
        Ok(projection.extensions.get(bundle).cloned())
    }

    async fn session_metadata(
        &self,
        session: SessionId,
    ) -> Result<Option<SessionSummaryMeta>, HostError> {
        let projection = self
            .store
            .read_projection_shared(session)
            .await
            .map_err(|error| HostError::Storage(error.to_string()))?;
        if projection.last_seq == 0 {
            return Ok(None);
        }
        Ok(Some(SessionSummaryMeta {
            session,
            parent: projection.session.parent,
            agent: projection.session.agent.clone(),
            model: projection.session.model.clone(),
            workdir: projection.session.workdir.clone(),
            title: projection.session.title.clone(),
            project: None,
            archived: projection.session.archived.is_some(),
            metadata: Value::Null,
            event_count: projection.last_seq,
        }))
    }

    async fn list_sessions(
        &self,
        _filter: SessionListFilter,
    ) -> Result<Vec<SessionSummaryMeta>, HostError> {
        Err(HostError::Unavailable(
            "list_sessions not implemented on AppHostRuntime".into(),
        ))
    }
}

#[async_trait]
impl HostBundleStorage for AppHostRuntime {
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
        Err(HostError::Unavailable(
            "bundle_storage not wired on AppHostRuntime".into(),
        ))
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
        Err(HostError::Unavailable(
            "bundle_storage claims not wired on AppHostRuntime".into(),
        ))
    }

    async fn renew_claim(
        &self,
        _lease: &BundleClaimLease,
        _ttl_millis: u64,
    ) -> Result<BundleClaimLease, HostError> {
        Err(HostError::Unavailable(
            "bundle_storage claims not wired on AppHostRuntime".into(),
        ))
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
impl HostTurnRunner for AppHostRuntime {
    async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
        Err(HostError::Unavailable(
            "spawn_session not wired on AppHostRuntime".into(),
        ))
    }

    async fn run_turn(
        &self,
        request: TurnStartRequest,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        if cancel.is_cancelled() {
            return Err(HostError::Cancelled);
        }
        let engine = self.engine()?;
        let projection = engine
            .read_projection_shared(request.session)
            .await
            .map_err(map_core_error)?;
        let spec = AgentSpec {
            name: request
                .agent
                .or_else(|| projection.session.agent.clone())
                .unwrap_or_else(|| self.default_agent.name.clone()),
            model: request
                .model
                .or_else(|| projection.session.model.clone())
                .unwrap_or_else(|| self.default_agent.model.clone()),
            system_prompt: request
                .system_override
                .unwrap_or_else(|| self.default_agent.system_prompt.clone()),
            workdir: projection
                .session
                .workdir
                .as_deref()
                .filter(|w| !w.is_empty())
                .map_or_else(|| self.default_agent.workdir.clone(), PathBuf::from),
            reasoning: self.default_agent.reasoning,
        };
        let prompt = engine
            .admit_user_prompt(request.session, request.prompt)
            .await
            .map_err(map_core_error)?;
        engine
            .run_turn_with_external_dirs_and_guidance(
                request.session,
                &spec,
                cancel,
                &[],
                None,
                None,
                Some(prompt),
            )
            .await
            .map_err(map_core_error)?;
        let updated = engine
            .read_projection_shared(request.session)
            .await
            .map_err(map_core_error)?;
        let last_assistant = updated
            .session
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant);
        let output_text = last_assistant
            .map(|msg| {
                msg.parts
                    .iter()
                    .filter_map(|part| match part {
                        PartProjection::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            })
            .unwrap_or_default();
        Ok(TurnOutcome {
            session: request.session,
            message_id: last_assistant.map(|msg| msg.id),
            finish_reason: last_assistant.and_then(|msg| msg.finish),
            output_text,
            error: None,
        })
    }

    async fn spawn_session_and_run(
        &self,
        _request: TurnSpawnRequest,
        _cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        Err(HostError::Unavailable(
            "spawn_session_and_run not wired on AppHostRuntime".into(),
        ))
    }

    async fn steer_turn(&self, _session: SessionId, _message: String) -> Result<(), HostError> {
        Err(HostError::Unavailable(
            "steer_turn not wired on AppHostRuntime".into(),
        ))
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
impl HostModelCaller for AppHostRuntime {
    async fn complete(
        &self,
        request: ModelCallRequest,
        cancel: CancellationToken,
    ) -> Result<ModelCallResponse, HostError> {
        if cancel.is_cancelled() {
            return Err(HostError::Cancelled);
        }
        let router = self
            .engine()
            .map(|engine| engine.provider_snapshot().router().clone())
            .unwrap_or_else(|_| (*self.router).clone());
        let model = request
            .model
            .unwrap_or_else(|| self.default_agent.model.clone());
        let completion = CompletionRequest {
            model: model.clone(),
            system: request.system,
            messages: request.messages,
            tools: request.tools,
            temperature: request.temperature,
            max_output_tokens: request.max_tokens,
            reasoning: request
                .reasoning_effort
                .as_deref()
                .and_then(ReasoningEffort::parse),
            headers: Default::default(),
        };
        let mut stream = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(HostError::Cancelled),
            res = router.stream(completion, SessionId::new(), MessageId::new()) => {
                res.map_err(|error| HostError::Unavailable(error.to_string()))?
            }
        };
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ModelToolCall> = Vec::new();
        let mut finish_reason = FinishReason::Stop;
        let mut usage: Option<TokenUsage> = None;
        loop {
            let item = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(HostError::Cancelled),
                next = stream.next() => next,
            };
            let Some(item) = item else {
                break;
            };
            let event = item.map_err(|error| HostError::Unavailable(error.to_string()))?;
            match event {
                Event::TextDelta { delta, .. } => text.push_str(&delta),
                Event::ReasoningDelta { delta, .. } => reasoning.push_str(&delta),
                Event::ToolCallRequested {
                    call, name, input, ..
                } => tool_calls.push(ModelToolCall {
                    id: call,
                    name,
                    arguments: input,
                }),
                Event::UsageRecorded { tokens, .. } => usage = Some(tokens),
                Event::MessageFinished { finish, tokens, .. } => {
                    finish_reason = finish;
                    if let Some(t) = tokens {
                        usage = Some(t);
                    }
                }
                _ => {}
            }
        }
        Ok(ModelCallResponse {
            model,
            text,
            reasoning: (!reasoning.is_empty()).then_some(reasoning),
            tool_calls,
            finish_reason,
            usage,
        })
    }

    async fn stream(
        &self,
        _request: ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
        Err(HostError::Unavailable(
            "model_caller.stream not wired on AppHostRuntime".into(),
        ))
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
impl HostEventBus for AppHostRuntime {
    fn publish(&self, envelope: Envelope) -> Result<(), HostError> {
        self.engine()?.bus().publish(envelope);
        Ok(())
    }

    fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
        Ok(self.engine()?.bus().subscribe())
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
impl HostCatalogReader for AppHostRuntime {
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
        let Ok(binding) = self.bind_for_workdir(&self.default_agent.workdir).await else {
            return Ok(Vec::new());
        };
        Ok(binding
            .bundle_catalog()
            .bundles()
            .iter()
            .map(|bundle| BundleCatalogSummary {
                identity: bundle.identity().clone(),
                enabled: true,
                description: None,
            })
            .collect())
    }

    async fn get_bundle(&self, id: &str) -> Result<Option<Bundle>, HostError> {
        if let Ok(binding) = self.bind_for_workdir(&self.default_agent.workdir).await {
            if let Some(bundle) = binding
                .bundle_catalog()
                .bundles()
                .iter()
                .find(|bundle| bundle.identity().id == id)
            {
                return Ok(Some(bundle.as_ref().clone()));
            }
            return Ok(None);
        }
        Ok(None)
    }

    async fn bundle_config(&self, bundle_id: &str) -> Result<Value, HostError> {
        Ok(crate::config::read_bundle_config_json(
            &self.config_file,
            bundle_id,
        ))
    }

    async fn global_config(&self) -> Result<Value, HostError> {
        Ok(Value::Null)
    }
}

#[async_trait]
impl HostExtensionRegistry for AppHostRuntime {
    async fn list_extensions(
        &self,
        kind: Option<ExtensionKind>,
    ) -> Result<Vec<ExtensionEntry>, HostError> {
        let mut entries = Vec::new();
        if (kind.is_none() || kind == Some(ExtensionKind::GoalDriver))
            && let Some(driver) = self.goal_driver.get()
        {
            entries.push(ExtensionEntry {
                bundle_id: "hya/goal".to_string(),
                kind: ExtensionKind::GoalDriver,
                name: driver.id().to_string(),
                metadata: Value::Null,
            });
        }
        Ok(entries)
    }

    async fn has_bundle(&self, bundle_id: &str) -> Result<bool, HostError> {
        Ok(self.get_bundle(bundle_id).await?.is_some())
    }

    async fn invoke_bundle_api(
        &self,
        _call: BundleApiInvocation,
    ) -> Result<BundleApiResult, HostError> {
        Err(HostError::Unavailable(
            "extension_registry.invoke_bundle_api not wired on AppHostRuntime".into(),
        ))
    }

    async fn invoke_tool(
        &self,
        _session: Option<SessionId>,
        _tool_name: &str,
        _input: Value,
        _cancel: CancellationToken,
    ) -> Result<Value, HostError> {
        Err(HostError::Unavailable(
            "extension_registry.invoke_tool not wired on AppHostRuntime".into(),
        ))
    }

    async fn has_goal_evaluate_hook(&self, session: Option<SessionId>) -> Result<bool, HostError> {
        let (workdir, agent) = self.resolve_session_scope(session).await?;
        let binding = self.bind_for_workdir(&workdir).await?;
        let dispatcher = goal_evaluator_hook_chain(&binding, &agent);
        Ok(dispatcher.has_goal_evaluate())
    }

    async fn invoke_goal_evaluate_hook(
        &self,
        session: Option<SessionId>,
        condition: &str,
        transcript: &str,
    ) -> Result<Option<NativeGoalEvaluateVerdict>, HostError> {
        let (workdir, agent) = self.resolve_session_scope(session).await?;
        let binding = self.bind_for_workdir(&workdir).await?;
        let dispatcher = goal_evaluator_hook_chain(&binding, &agent);
        if !dispatcher.has_goal_evaluate() {
            return Ok(None);
        }
        match dispatcher.goal_evaluate(condition, transcript).await {
            Ok(GoalEvaluateReply::Verdict { met, reason }) => {
                Ok(Some(NativeGoalEvaluateVerdict::Verdict { met, reason }))
            }
            Ok(GoalEvaluateReply::Malformed) => Ok(Some(NativeGoalEvaluateVerdict::Malformed)),
            Err(error) => Err(map_core_error(error)),
        }
    }
}

impl HostRuntime for AppHostRuntime {
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

    fn goal_driver(&self) -> Option<Arc<dyn GoalDriver>> {
        self.goal_driver.get().cloned()
    }
}
