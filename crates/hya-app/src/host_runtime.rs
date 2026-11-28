//! App-layer [`hya_runtime_abi::HostRuntime`] implementation injected into
//! ABI v2 native bundles (`hya/coding-extras`, `hya/hashline`, `hya/goal`, `hya/loop`, etc.).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use futures::StreamExt as _;
use hya_bundle::{Bundle, PreparedAgent};
use hya_engine::hooks::{EvidenceQuality, GoalEvaluateReply, HookDispatcher, VerifierVerdict};
use hya_engine::resident::ActivationResult;
use hya_engine::{
    AgentSpec, BundleApiCall, BundleApiError, CatalogScope, CategoryRegistry, CoreError,
    CreateSession, ExplicitModelRoute, ExplicitModelRouteCandidate, ExplicitTurnRoute, HookChain,
    ResidentSupervisor, SessionEngine, SessionStoreExt as _, SidecarEnvironment as _, TurnBinding,
    apply_agent_model_preference, apply_spawn_model_policy,
};
use hya_proto::{
    ActorClaim, AgentName, ChannelPolicySnapshot, Envelope, Event, EventSeq, FinishReason, Message,
    MessageId, ModelRef, OwnerRunId, PartProjection, Projection, Role, SessionId, SessionKind,
    TokenUsage,
};
use hya_provider::{CompletionRequest, ProviderRouter, ReasoningEffort};
use hya_runtime_abi::{
    BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
    BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionKind, GoalDriver,
    HostBundleStorage, HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry,
    HostModelCaller, HostRuntime, HostSessionStore, HostTurnRunner, LoopDriver,
    ManagedMemberActivateRequest, ManagedMemberActivationOutcome, ManagedMemberActivationStatus,
    ManagedMemberHandle, ManagedMemberSpawnRequest, MemberMailRevivalRequest, ModelCallRequest,
    ModelCallResponse, ModelRouteEntry, ModelStreamChunk, ModelToolCall, ModelWindowInfo,
    NativeGoalEvaluateVerdict, NativeLoopPlanInput, NativePlannerOutput, NativeVerifierVerdict,
    ResolvedModelRoute, ResolvedModelRouteCandidate, ScopeCatalogSnapshot, SessionListFilter,
    SessionSummaryMeta, StagedMailAck, TurnOutcome, TurnSpawnRequest, TurnStartRequest, TurnStatus,
};
use hya_store::SessionStore;
use hya_tool::{AgentDef, ReportOutcome, SkillCatalogEntry, SpawnMember};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::bundle_runtime::sidecar::environment::BundleSidecarEnvironment;

/// [`HostRuntime`] backed by `hya-app`'s [`SessionStore`], [`ProviderRouter`],
/// active `config.yaml` path, and late-attached [`SessionEngine`] + native drivers.
pub(crate) struct AppHostRuntime {
    store: SessionStore,
    owner_run_id: OwnerRunId,
    config_file: PathBuf,
    router: Arc<ProviderRouter>,
    default_agent: AgentSpec,
    categories: Arc<CategoryRegistry>,
    tokio_handle: tokio::runtime::Handle,
    engine: OnceLock<Weak<SessionEngine>>,
    resident_supervisor: OnceLock<Weak<ResidentSupervisor>>,
    sidecar_environment: OnceLock<Arc<BundleSidecarEnvironment>>,
    goal_driver: OnceLock<Arc<dyn GoalDriver>>,
    loop_drivers: OnceLock<Vec<Arc<dyn LoopDriver>>>,
}

impl AppHostRuntime {
    /// Construct a host runtime for native bundles before [`SessionEngine`] is built.
    pub(crate) fn new(
        store: SessionStore,
        owner_run_id: OwnerRunId,
        config_file: PathBuf,
        router: Arc<ProviderRouter>,
        default_agent: AgentSpec,
        categories: Arc<CategoryRegistry>,
    ) -> Self {
        Self {
            store,
            owner_run_id,
            config_file,
            router,
            default_agent,
            categories,
            tokio_handle: tokio::runtime::Handle::current(),
            engine: OnceLock::new(),
            resident_supervisor: OnceLock::new(),
            sidecar_environment: OnceLock::new(),
            goal_driver: OnceLock::new(),
            loop_drivers: OnceLock::new(),
        }
    }

    /// Poll `future` (and drop it on cancellation) with the host's Tokio runtime
    /// context entered, so ABI v2 native bundle tasks running on a `cdylib`'s
    /// internal Tokio pool can safely invoke host `sqlx`/engine futures.
    fn on_host<F: std::future::Future>(&self, future: F) -> HostContextFuture<F> {
        HostContextFuture {
            handle: self.tokio_handle.clone(),
            future: std::mem::ManuallyDrop::new(future),
        }
    }

    /// Attach the constructed [`SessionEngine`] via a weak reference (avoiding
    /// an `Arc` cycle while enabling turn execution, hooks, catalog, and bus).
    pub(crate) fn attach_engine(&self, engine: &Arc<SessionEngine>) {
        let _ = self.engine.set(Arc::downgrade(engine));
    }

    /// Attach the constructed [`ResidentSupervisor`] via a weak reference.
    pub(crate) fn attach_resident_supervisor(&self, supervisor: &Arc<ResidentSupervisor>) {
        let _ = self.resident_supervisor.set(Arc::downgrade(supervisor));
    }

    /// Attach the sidecar environment used when spawning bundle-owned agents.
    pub(crate) fn attach_sidecar_environment(&self, sidecar: Arc<BundleSidecarEnvironment>) {
        let _ = self.sidecar_environment.set(sidecar);
    }

    /// Register the active native [`GoalDriver`] loaded from `hya/goal`.
    pub(crate) fn attach_goal_driver(&self, driver: Arc<dyn GoalDriver>) {
        let _ = self.goal_driver.set(driver);
    }

    /// Register loaded native [`LoopDriver`] instances (e.g. from `hya/loop`).
    pub(crate) fn set_loop_drivers(&self, drivers: Vec<Arc<dyn LoopDriver>>) {
        let _ = self.loop_drivers.set(drivers);
    }

    fn engine(&self) -> Result<Arc<SessionEngine>, HostError> {
        self.engine
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| HostError::Unavailable("session engine is not attached".into()))
    }

    fn supervisor(&self) -> Result<Arc<ResidentSupervisor>, HostError> {
        self.resident_supervisor
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| HostError::Unavailable("resident supervisor is not attached".into()))
    }

    async fn session_workdir_strict(&self, session: SessionId) -> Result<PathBuf, HostError> {
        let projection = self.read_projection(session).await?;
        match (projection.session.id, projection.session.workdir.as_deref()) {
            (Some(_), Some(workdir)) => Ok(PathBuf::from(workdir)),
            _ => Err(HostError::NotFound(format!(
                "Session `{session}` was not found"
            ))),
        }
    }

    fn resolve_managed_member_runtime(
        &self,
        engine: &SessionEngine,
        binding: &TurnBinding,
        workdir: &Path,
        caller: Option<&AgentName>,
        agent: &AgentName,
        route: Option<&ResolvedModelRoute>,
    ) -> Result<(AgentSpec, hya_engine::resident::ResolvedResidentRuntime), HostError> {
        let definition = match caller {
            Some(caller) => binding
                .resolve_workflow_agent(caller.as_str(), agent.as_str())
                .map_err(|error| HostError::PermissionDenied(error.to_string()))?,
            None => binding.resolve_agent(agent.as_str()).ok_or_else(|| {
                HostError::PermissionDenied(format!("unknown agent `{}`", agent.as_str()))
            })?,
        };
        let mut base = self.default_agent.clone();
        base.workdir = workdir.to_path_buf();
        let mut spec = engine
            .agent_spec_for_binding(binding, &base, definition.stable_id)
            .map_err(map_core_error)?;
        let providers = engine.provider_snapshot();
        let is_servable = |model: &ModelRef| providers.router().resolve(model).is_some();
        spec = apply_agent_model_preference(
            spec,
            &definition,
            binding.agent_model_preference(definition.stable_id),
            &is_servable,
        );
        let empty_member = SpawnMember {
            description: String::new(),
            prompt: String::new(),
            subagent_type: definition.stable_id.to_string(),
            model: None,
            category: None,
            inline_agent: None,
            effort: None,
        };
        spec = apply_spawn_model_policy(
            spec,
            &definition,
            &empty_member,
            &self.categories,
            &is_servable,
        );
        if let Some(selected) = route.and_then(ResolvedModelRoute::selected) {
            spec.model = selected.model.clone();
            spec.reasoning = ReasoningEffort::parse(&selected.reasoning);
        }
        let roster = engine
            .agent_roster_for_binding(binding, definition.stable_id)
            .map_err(map_core_error)?;
        let resources = engine
            .agent_resource_policy_for_binding(binding, definition.stable_id)
            .map_err(map_core_error)?;
        let sidecar = match self.sidecar_environment.get() {
            Some(env) => env
                .factory_for(binding, definition.stable_id)
                .map_err(map_core_error)?,
            None => None,
        };
        Ok((spec, (binding.clone(), roster, resources, sidecar)))
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

    async fn loop_hooks_for_session(
        &self,
        session: SessionId,
    ) -> Result<Arc<dyn HookDispatcher>, HostError> {
        let engine = self.engine()?;
        let projection = self.read_projection(session).await?;
        let workdir = projection
            .session
            .workdir
            .as_deref()
            .filter(|w| !w.is_empty())
            .map_or_else(|| self.default_agent.workdir.clone(), PathBuf::from);
        let agent_name = projection
            .session
            .agent
            .as_ref()
            .map_or(self.default_agent.name.as_str(), AgentName::as_str);
        let binding = engine
            .bind_session_runtime(session, &workdir)
            .await
            .map_err(map_core_error)?;
        Ok(build_loop_hook_chain(&binding, agent_name))
    }
}

struct HostContextFuture<F> {
    handle: tokio::runtime::Handle,
    future: std::mem::ManuallyDrop<F>,
}

impl<F: std::future::Future> std::future::Future for HostContextFuture<F> {
    type Output = F::Output;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        // SAFETY: `future` is never moved after pinning and is dropped in-place inside `Drop`.
        let this = unsafe { self.get_unchecked_mut() };
        let _guard = this.handle.enter();
        // SAFETY: `this.future` is structurally pinned while `HostContextFuture` is pinned.
        let pinned = unsafe { std::pin::Pin::new_unchecked(&mut *this.future) };
        pinned.poll(cx)
    }
}

impl<F> Drop for HostContextFuture<F> {
    fn drop(&mut self) {
        let _guard = self.handle.enter();
        // SAFETY: `future` is only dropped here once.
        unsafe { std::mem::ManuallyDrop::drop(&mut self.future) };
    }
}

fn map_core_error(error: CoreError) -> HostError {
    match error {
        CoreError::Cancelled => HostError::Cancelled,
        CoreError::Invalid(msg) => HostError::InvalidInput(msg),
        CoreError::Store(err) => HostError::Storage(err.to_string()),
        CoreError::MailAuthorizationRejected { .. } => {
            HostError::PermissionDenied(error.to_string())
        }
        CoreError::MailboxRejected(_) | CoreError::MailRevivalRequired { .. } => {
            HostError::Conflict(error.to_string())
        }
        CoreError::CapacityExceeded { .. } | CoreError::TurnAlreadyActive { .. } => {
            HostError::Unavailable(error.to_string())
        }
        other => HostError::Internal(other.to_string()),
    }
}

fn explicit_route_from_resolved(route: &ResolvedModelRoute) -> ExplicitTurnRoute {
    let candidates = route
        .candidates
        .iter()
        .map(|candidate| ExplicitModelRouteCandidate {
            model: candidate.model.clone(),
            reasoning: ReasoningEffort::parse(&candidate.reasoning).unwrap_or(ReasoningEffort::Off),
        })
        .collect();
    ExplicitTurnRoute::new(ExplicitModelRoute {
        candidates,
        selected_index: route.selected_index,
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
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

pub(crate) fn build_loop_hook_chain(
    binding: &TurnBinding,
    stable_agent_id: &str,
) -> Arc<dyn HookDispatcher> {
    let active_is_loop_owner = binding.resolve_agent(stable_agent_id).is_some_and(|agent| {
        matches!(agent.origin.bundle_id(), Some("hya/loop" | "hya/goal-loop"))
    });
    let selected = binding.bundle_hooks_for_agent(if active_is_loop_owner {
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
                && !matches!(bundle.identity().id.as_str(), "hya/loop" | "hya/goal-loop")
        })
        .filter_map(|bundle| binding.bundle_hooks(&bundle.identity().id))
        .collect::<Vec<_>>();
    let mut hooks: Vec<Arc<dyn HookDispatcher>> = Vec::new();
    for hook in binding
        .global_plugin_hooks()
        .into_iter()
        .chain(agentless)
        .chain(selected)
        .chain(binding.bundle_hooks("hya/goal-loop"))
        .chain(binding.bundle_hooks("hya/loop"))
    {
        if !hooks.iter().any(|existing| Arc::ptr_eq(existing, &hook)) {
            hooks.push(hook);
        }
    }
    Arc::new(HookChain::new(hooks))
}

fn extract_turn_outcome(session: SessionId, projection: &Projection) -> TurnOutcome {
    let last_assistant = projection
        .session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant);
    match last_assistant {
        Some(message) => {
            let mut output_text = String::new();
            for part in &message.parts {
                if let PartProjection::Text { text, .. } = part {
                    output_text.push_str(text);
                }
            }
            TurnOutcome {
                session,
                message_id: Some(message.id),
                finish_reason: message.finish,
                output_text,
                error: None,
            }
        }
        None => TurnOutcome {
            session,
            message_id: None,
            finish_reason: None,
            output_text: String::new(),
            error: None,
        },
    }
}

pub(crate) async fn complete_with_router(
    router: &ProviderRouter,
    default_model: &ModelRef,
    request: ModelCallRequest,
    cancel: CancellationToken,
) -> Result<ModelCallResponse, HostError> {
    if cancel.is_cancelled() {
        return Err(HostError::Cancelled);
    }
    let model = request.model.unwrap_or_else(|| default_model.clone());
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
    let mut tool_calls = Vec::new();
    let mut finish_reason = FinishReason::Stop;
    let mut usage: Option<TokenUsage> = None;
    loop {
        let next = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(HostError::Cancelled),
            item = stream.next() => item,
        };
        let Some(item) = next else {
            break;
        };
        match item.map_err(|error| HostError::Unavailable(error.to_string()))? {
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

#[async_trait]
impl HostSessionStore for AppHostRuntime {
    async fn session_exists(&self, session: SessionId) -> Result<bool, HostError> {
        self.on_host(async move {
            self.store
                .read_projection_shared(session)
                .await
                .map(|projection| projection.last_seq > 0)
                .map_err(|error| HostError::Storage(error.to_string()))
        })
        .await
    }

    async fn read_events(
        &self,
        session: SessionId,
        after_seq: Option<EventSeq>,
        limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError> {
        self.on_host(async move {
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
        })
        .await
    }

    async fn append_events(
        &self,
        session: SessionId,
        claim: Option<ActorClaim>,
        events: Vec<Event>,
    ) -> Result<Vec<Envelope>, HostError> {
        self.on_host(async move {
            let appended = if let Some(claim) = claim {
                self.store
                    .commit_resident_mutation(&claim, session, events)
                    .await
                    .map_err(map_core_error)?
            } else {
                self.store
                    .append_events(session, events)
                    .await
                    .map_err(|error| HostError::Storage(error.to_string()))?
            };
            if let Ok(engine) = self.engine() {
                for env in &appended {
                    engine.bus().publish(env.clone());
                }
            }
            Ok(appended)
        })
        .await
    }

    async fn append_extension_event(
        &self,
        session: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
        claim: Option<ActorClaim>,
    ) -> Result<Envelope, HostError> {
        let event = Event::Extension {
            session,
            bundle: bundle.to_string(),
            kind: kind.to_string(),
            payload,
        };
        let mut appended = self.append_events(session, claim, vec![event]).await?;
        appended.pop().ok_or_else(|| {
            HostError::Internal("append_extension_event produced no envelope".into())
        })
    }

    async fn read_projection(&self, session: SessionId) -> Result<Arc<Projection>, HostError> {
        self.on_host(async move {
            self.store
                .read_projection_shared(session)
                .await
                .map_err(|error| HostError::Storage(error.to_string()))
        })
        .await
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
        let projection = self.read_projection(session).await?;
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

    fn runtime_owner_id(&self) -> Option<OwnerRunId> {
        Some(self.owner_run_id)
    }

    async fn find_sessions_with_extension(
        &self,
        bundle: &str,
        kind: &str,
    ) -> Result<Vec<SessionId>, HostError> {
        let bundle = bundle.to_owned();
        let kind = kind.to_owned();
        self.on_host(async move {
            let mut tx = self
                .store
                .begin_transaction()
                .await
                .map_err(|error| HostError::Storage(error.to_string()))?;
            let sessions =
                SessionStore::find_sessions_with_extension_event(&mut tx, &bundle, &kind)
                    .await
                    .map_err(|error| HostError::Storage(error.to_string()))?;
            tx.commit()
                .await
                .map_err(|error| HostError::Storage(error.to_string()))?;
            Ok(sessions)
        })
        .await
    }
}

fn scoped_state_key(scope: &str, key: &str) -> String {
    if scope.is_empty() {
        key.to_string()
    } else {
        format!("{scope}:{key}")
    }
}

fn strip_scope_prefix<'a>(scope: &str, stored_key: &'a str) -> Option<&'a str> {
    if scope.is_empty() {
        Some(stored_key)
    } else {
        stored_key.strip_prefix(&format!("{scope}:"))
    }
}

fn store_claim_to_lease(row: hya_store::BundleClaim) -> BundleClaimLease {
    BundleClaimLease {
        bundle: row.bundle,
        resource_key: row.claim_key,
        owner_id: row.owner,
        epoch: row.epoch,
        expires_at_millis: row.expires_at.unwrap_or(i64::MAX),
        metadata: row.metadata,
    }
}

fn lease_to_store_claim(lease: &BundleClaimLease) -> hya_store::BundleClaim {
    hya_store::BundleClaim {
        bundle: lease.bundle.clone(),
        claim_key: lease.resource_key.clone(),
        owner: lease.owner_id.clone(),
        epoch: lease.epoch,
        expires_at: (lease.expires_at_millis != i64::MAX).then_some(lease.expires_at_millis),
        metadata: lease.metadata.clone(),
        acquired_at: 0,
        updated_at: 0,
    }
}

#[async_trait]
impl HostBundleStorage for AppHostRuntime {
    async fn get_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
    ) -> Result<Option<BundleStateEntry>, HostError> {
        let bundle = bundle.to_owned();
        let scope = scope.to_owned();
        let key = key.to_owned();
        self.on_host(async move {
            let stored_key = scoped_state_key(&scope, &key);
            let row = self
                .store
                .get_bundle_state_entry(&bundle, &stored_key)
                .await
                .map_err(|error| HostError::Storage(error.to_string()))?;
            row.map(|row| {
                let value = serde_json::from_slice(&row.value)
                    .map_err(|error| HostError::Storage(error.to_string()))?;
                Ok(BundleStateEntry {
                    bundle: row.bundle,
                    scope,
                    key,
                    value,
                    version: 1,
                    updated_at_millis: row.updated_at,
                })
            })
            .transpose()
        })
        .await
    }

    async fn put_state(
        &self,
        bundle: &str,
        scope: &str,
        key: &str,
        value: Value,
        _expected_version: Option<u64>,
    ) -> Result<BundleStateEntry, HostError> {
        let bundle = bundle.to_owned();
        let scope = scope.to_owned();
        let key = key.to_owned();
        self.on_host(async move {
            let stored_key = scoped_state_key(&scope, &key);
            self.store
                .set_bundle_state_json(&bundle, &stored_key, &value)
                .await
                .map_err(|error| HostError::Storage(error.to_string()))?;
            Ok(BundleStateEntry {
                bundle,
                scope,
                key,
                value,
                version: 1,
                updated_at_millis: hya_proto::now_millis(),
            })
        })
        .await
    }

    async fn delete_state(&self, bundle: &str, scope: &str, key: &str) -> Result<bool, HostError> {
        let bundle = bundle.to_owned();
        let stored_key = scoped_state_key(scope, key);
        self.on_host(async move {
            self.store
                .delete_bundle_state(&bundle, &stored_key)
                .await
                .map_err(|error| HostError::Storage(error.to_string()))
        })
        .await
    }

    async fn list_state(
        &self,
        bundle: &str,
        scope: &str,
        prefix: Option<&str>,
    ) -> Result<Vec<BundleStateEntry>, HostError> {
        let bundle = bundle.to_owned();
        let scope = scope.to_owned();
        let full_prefix = match (scope.is_empty(), prefix) {
            (true, p) => p.map(str::to_owned),
            (false, Some(p)) => Some(format!("{scope}:{p}")),
            (false, None) => Some(format!("{scope}:")),
        };
        self.on_host(async move {
            let rows = self
                .store
                .list_bundle_state(&bundle, full_prefix.as_deref())
                .await
                .map_err(|error| HostError::Storage(error.to_string()))?;
            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                let Some(unscoped_key) = strip_scope_prefix(&scope, &row.key) else {
                    continue;
                };
                let value = serde_json::from_slice(&row.value)
                    .map_err(|error| HostError::Storage(error.to_string()))?;
                out.push(BundleStateEntry {
                    bundle: row.bundle,
                    scope: scope.clone(),
                    key: unscoped_key.to_string(),
                    value,
                    version: 1,
                    updated_at_millis: row.updated_at,
                });
            }
            Ok(out)
        })
        .await
    }

    async fn acquire_claim(
        &self,
        request: BundleClaimRequest,
    ) -> Result<BundleClaimLease, HostError> {
        self.on_host(async move {
            let ttl = (request.ttl_millis > 0).then_some(request.ttl_millis);
            let row = self
                .store
                .acquire_bundle_claim(
                    &request.bundle,
                    &request.resource_key,
                    &request.owner_id,
                    ttl,
                    Some(&request.metadata),
                )
                .await
                .map_err(|error| HostError::Storage(error.to_string()))?;
            Ok(store_claim_to_lease(row))
        })
        .await
    }

    async fn renew_claim(
        &self,
        lease: &BundleClaimLease,
        ttl_millis: u64,
    ) -> Result<BundleClaimLease, HostError> {
        let claim = lease_to_store_claim(lease);
        self.on_host(async move {
            let ttl = (ttl_millis > 0).then_some(ttl_millis);
            let row = self
                .store
                .renew_bundle_claim(&claim, ttl, None)
                .await
                .map_err(|error| HostError::Storage(error.to_string()))?;
            Ok(store_claim_to_lease(row))
        })
        .await
    }

    async fn release_claim(&self, lease: &BundleClaimLease) -> Result<(), HostError> {
        let claim = lease_to_store_claim(lease);
        self.on_host(async move {
            self.store
                .release_bundle_claim(&claim)
                .await
                .map_err(|error| HostError::Storage(error.to_string()))
        })
        .await
    }

    async fn read_claim(
        &self,
        bundle: &str,
        resource_key: &str,
    ) -> Result<Option<BundleClaimLease>, HostError> {
        let bundle = bundle.to_owned();
        let resource_key = resource_key.to_owned();
        self.on_host(async move {
            let row = self
                .store
                .get_bundle_claim_at(&bundle, &resource_key, hya_proto::now_millis())
                .await
                .map_err(|error| HostError::Storage(error.to_string()))?;
            Ok(row.map(store_claim_to_lease))
        })
        .await
    }

    async fn list_claims(&self, bundle: &str) -> Result<Vec<BundleClaimLease>, HostError> {
        let bundle = bundle.to_owned();
        self.on_host(async move {
            let rows = self
                .store
                .list_active_bundle_claims_at(&bundle, hya_proto::now_millis())
                .await
                .map_err(|error| HostError::Storage(error.to_string()))?;
            Ok(rows.into_iter().map(store_claim_to_lease).collect())
        })
        .await
    }
}

#[async_trait]
impl HostTurnRunner for AppHostRuntime {
    async fn spawn_session(&self, request: TurnSpawnRequest) -> Result<SessionId, HostError> {
        let engine = self.engine()?;
        let parent_meta = match request.parent {
            Some(parent) => self.session_metadata(parent).await.ok().flatten(),
            None => None,
        };
        let workdir = request
            .workdir
            .or_else(|| {
                parent_meta
                    .as_ref()
                    .and_then(|meta| meta.workdir.as_ref())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| self.default_agent.workdir.clone());
        let model = request
            .model
            .or_else(|| parent_meta.as_ref().and_then(|meta| meta.model.clone()))
            .unwrap_or_else(|| self.default_agent.model.clone());
        engine
            .create(CreateSession {
                parent: request.parent,
                agent: request.agent,
                model,
                workdir: workdir.to_string_lossy().into_owned(),
                project: None,
                kind: SessionKind::Project,
            })
            .await
            .map_err(map_core_error)
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
        let meta = self.session_metadata(request.session).await?;
        let workdir = meta
            .as_ref()
            .and_then(|m| m.workdir.as_deref())
            .filter(|w| !w.is_empty())
            .map_or_else(|| self.default_agent.workdir.clone(), PathBuf::from);
        let agent_name = request
            .agent
            .or_else(|| meta.as_ref().and_then(|m| m.agent.clone()))
            .unwrap_or_else(|| self.default_agent.name.clone());
        let model = request
            .model
            .or_else(|| meta.as_ref().and_then(|m| m.model.clone()))
            .unwrap_or_else(|| self.default_agent.model.clone());
        let binding = engine
            .bind_session_runtime(request.session, &workdir)
            .await
            .map_err(map_core_error)?;
        engine
            .admit_user_prompt_with_binding(
                request.session,
                request.prompt,
                &binding,
                agent_name.as_str(),
            )
            .await
            .map_err(map_core_error)?;
        let spec = AgentSpec {
            name: agent_name,
            model,
            system_prompt: request
                .system_override
                .unwrap_or_else(|| self.default_agent.system_prompt.clone()),
            workdir,
            reasoning: self.default_agent.reasoning,
        };
        engine
            .run_bound_turn(request.session, &spec, binding, cancel, None)
            .await
            .map_err(map_core_error)?;
        let projection = engine
            .read_projection_shared(request.session)
            .await
            .map_err(map_core_error)?;
        Ok(extract_turn_outcome(request.session, &projection))
    }

    async fn spawn_session_and_run(
        &self,
        request: TurnSpawnRequest,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, HostError> {
        if cancel.is_cancelled() {
            return Err(HostError::Cancelled);
        }
        let engine = self.engine()?;
        let parent_meta = match request.parent {
            Some(parent) => self.session_metadata(parent).await.ok().flatten(),
            None => None,
        };
        let workdir = request
            .workdir
            .or_else(|| {
                parent_meta
                    .as_ref()
                    .and_then(|meta| meta.workdir.as_ref())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| self.default_agent.workdir.clone());
        let model = request
            .model
            .or_else(|| parent_meta.as_ref().and_then(|meta| meta.model.clone()))
            .unwrap_or_else(|| self.default_agent.model.clone());
        let bind_target = request.parent;
        let child = engine
            .create(CreateSession {
                parent: request.parent,
                agent: request.agent.clone(),
                model: model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
                project: None,
                kind: SessionKind::Project,
            })
            .await
            .map_err(map_core_error)?;
        let binding = engine
            .bind_session_runtime(bind_target.unwrap_or(child), &workdir)
            .await
            .map_err(map_core_error)?;
        engine
            .capture_session_bundle_hooks(child, &binding, request.agent.as_str())
            .await;
        engine
            .admit_user_prompt_with_binding(child, request.prompt, &binding, request.agent.as_str())
            .await
            .map_err(map_core_error)?;
        let spec = AgentSpec {
            name: request.agent,
            model,
            system_prompt: self.default_agent.system_prompt.clone(),
            workdir,
            reasoning: self.default_agent.reasoning,
        };
        engine
            .run_bound_turn(child, &spec, binding, cancel, None)
            .await
            .map_err(map_core_error)?;
        let projection = engine
            .read_projection_shared(child)
            .await
            .map_err(map_core_error)?;
        Ok(extract_turn_outcome(child, &projection))
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

    async fn preflight_member_capacity(
        &self,
        parent: SessionId,
        wanted: u64,
    ) -> Result<(), HostError> {
        self.on_host(async move {
            let engine = self.engine()?;
            let (root, depth) = engine
                .session_lineage(parent)
                .await
                .map_err(map_core_error)?;
            let Some(governor) = engine.governor() else {
                return Ok(());
            };
            let limits = governor.limits();
            if depth.saturating_add(1) > governor.max_depth() {
                return Err(HostError::InvalidInput(format!(
                    "subagent max depth {} reached",
                    governor.max_depth()
                )));
            }
            let live = engine
                .store()
                .live_member_count(root)
                .await
                .map_err(map_core_error)?;
            if live.saturating_add(wanted) > limits.per_run_budget {
                return Err(HostError::Unavailable(format!(
                    "subagent per-run budget exceeded (live={live}, wanted={wanted}, budget={})",
                    limits.per_run_budget
                )));
            }
            Ok(())
        })
        .await
    }

    async fn begin_root_activity(
        &self,
        parent: SessionId,
    ) -> Result<Arc<dyn Send + Sync>, HostError> {
        self.on_host(async move {
            let supervisor = self.supervisor()?;
            let guard = supervisor
                .root_driver_activity(parent)
                .await
                .map_err(map_core_error)?;
            Ok(Arc::new(guard) as Arc<dyn Send + Sync>)
        })
        .await
    }

    async fn spawn_managed_member(
        &self,
        request: ManagedMemberSpawnRequest,
    ) -> Result<ManagedMemberHandle, HostError> {
        self.on_host(async move {
            let engine = self.engine()?;
            let supervisor = self.supervisor()?;
            let workdir = self.session_workdir_strict(request.parent).await?;
            let binding = engine
                .bind_session_runtime(request.parent, &workdir)
                .await
                .map_err(map_core_error)?;
            let (spec, resolved) = self.resolve_managed_member_runtime(
                &engine,
                &binding,
                &workdir,
                Some(&request.caller),
                &request.agent,
                request.route.as_ref(),
            )?;
            let guidance: Option<Arc<str>> = request.system_context.map(Arc::from);
            let (session, handle, member) = supervisor
                .spawn_resident_parked(
                    request.parent,
                    spec,
                    resolved,
                    request.label,
                    request.actor_claim.as_ref(),
                    guidance,
                )
                .await
                .map_err(map_core_error)?;
            Ok(ManagedMemberHandle {
                session,
                handle,
                member,
            })
        })
        .await
    }

    async fn activate_managed_member(
        &self,
        request: ManagedMemberActivateRequest,
        cancel: CancellationToken,
    ) -> Result<ManagedMemberActivationOutcome, HostError> {
        self.on_host(async move {
            if cancel.is_cancelled() {
                return Ok(ManagedMemberActivationOutcome {
                    status: ManagedMemberActivationStatus::Cancelled,
                    output_text: String::new(),
                    route_outcomes: Vec::new(),
                });
            }
            let engine = self.engine()?;
            let supervisor = self.supervisor()?;
            let workdir = self.session_workdir_strict(request.parent).await?;
            let binding = engine
                .bind_session_runtime(request.parent, &workdir)
                .await
                .map_err(map_core_error)?;
            let (spec, resolved) = self.resolve_managed_member_runtime(
                &engine,
                &binding,
                &workdir,
                None,
                &request.agent,
                request.route.as_ref(),
            )?;
            let explicit_route = request.route.as_ref().map(explicit_route_from_resolved);
            let mut waiter = supervisor
                .activate(
                    request.parent,
                    request.member.session,
                    (spec, resolved),
                    request.directive,
                    Arc::from(request.system_context),
                    explicit_route.clone(),
                    request.actor_claim.as_ref(),
                )
                .await
                .map_err(map_core_error)?;
            let outcome = tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    let _ = self.end_managed_member(request.parent, &request.member.handle).await;
                    ManagedMemberActivationOutcome {
                        status: ManagedMemberActivationStatus::Cancelled,
                        output_text: String::new(),
                        route_outcomes: explicit_route
                            .as_ref()
                            .map_or_else(Vec::new, ExplicitTurnRoute::outcomes),
                    }
                }
                res = waiter.wait() => {
                    let route_outcomes = explicit_route
                        .as_ref()
                        .map_or_else(Vec::new, ExplicitTurnRoute::outcomes);
                    match res.map_err(map_core_error)? {
                        ActivationResult::Settled { final_text } => ManagedMemberActivationOutcome {
                            status: ManagedMemberActivationStatus::Done,
                            output_text: final_text,
                            route_outcomes,
                        },
                        ActivationResult::Reported { outcome, text } => ManagedMemberActivationOutcome {
                            status: match outcome {
                                ReportOutcome::Done => ManagedMemberActivationStatus::Done,
                                ReportOutcome::Failed => ManagedMemberActivationStatus::Failed,
                            },
                            output_text: text,
                            route_outcomes,
                        },
                        ActivationResult::Ended { status, note, .. } => ManagedMemberActivationOutcome {
                            status: match status {
                                hya_proto::MemberRunStatus::Cancelled => {
                                    ManagedMemberActivationStatus::Cancelled
                                }
                                _ => ManagedMemberActivationStatus::Failed,
                            },
                            output_text: note,
                            route_outcomes,
                        },
                    }
                }
            };
            Ok(outcome)
        })
        .await
    }

    async fn end_managed_member(&self, parent: SessionId, handle: &str) -> Result<(), HostError> {
        let handle = handle.to_owned();
        self.on_host(async move {
            let engine = self.engine()?;
            let supervisor = self.supervisor()?;
            let (root, _) = engine
                .session_lineage(parent)
                .await
                .map_err(map_core_error)?;
            supervisor
                .end_member(root, &handle)
                .await
                .map_err(map_core_error)
        })
        .await
    }

    async fn session_channel_policy(
        &self,
        session: SessionId,
    ) -> Result<Option<ChannelPolicySnapshot>, HostError> {
        Ok(self
            .engine()
            .ok()
            .and_then(|engine| engine.session_channel_policy(session)))
    }

    fn mail_mark(&self, session: SessionId) -> Option<u64> {
        self.engine()
            .ok()
            .and_then(|engine| engine.mail_mark(session))
    }

    fn notify_mail_consumed(&self, session: SessionId, mark: u64, consumed_incoming: bool) {
        if let Ok(engine) = self.engine() {
            engine.notify_mail_consumed(session, Some(mark), consumed_incoming);
        }
    }

    fn is_member_stopped_by_user(&self, root: SessionId, handle: &str) -> bool {
        self.engine()
            .ok()
            .is_some_and(|engine| engine.is_member_stopped_by_user(root, handle))
    }

    fn is_mail_steer_suppressed(&self, session: SessionId) -> bool {
        self.engine()
            .ok()
            .is_some_and(|engine| engine.is_mail_steer_suppressed(session))
    }

    fn stage_mail_steer_ack(&self, session: SessionId, ack: StagedMailAck) {
        if let Ok(engine) = self.engine() {
            engine.stage_mail_steer_ack(session, ack);
        }
    }

    async fn revive_archived_member_with_mail(
        &self,
        request: MemberMailRevivalRequest,
    ) -> Result<usize, HostError> {
        self.on_host(async move {
            let engine = self.engine()?;
            engine
                .revive_archived_member_with_mail(request)
                .await
                .map_err(map_core_error)
        })
        .await
    }

    async fn archive_subagent(
        &self,
        child: SessionId,
    ) -> Result<hya_tool::ArchiveReceipt, HostError> {
        self.on_host(async move {
            let engine = self.engine()?;
            engine.archive_subagent(child).await.map_err(map_core_error)
        })
        .await
    }

    async fn steer_subagent(
        &self,
        child: SessionId,
        text: &str,
    ) -> Result<hya_tool::MailReceipt, HostError> {
        let text = text.to_owned();
        self.on_host(async move {
            let engine = self.engine()?;
            engine
                .steer_subagent(child, &text)
                .await
                .map_err(map_core_error)
        })
        .await
    }
}

#[async_trait]
impl HostModelCaller for AppHostRuntime {
    async fn complete(
        &self,
        request: ModelCallRequest,
        cancel: CancellationToken,
    ) -> Result<ModelCallResponse, HostError> {
        let router = self
            .engine()
            .map(|engine| engine.provider_snapshot().router().clone())
            .unwrap_or_else(|_| (*self.router).clone());
        complete_with_router(&router, &self.default_agent.model, request, cancel).await
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

    async fn resolve_model_route(
        &self,
        entries: &[ModelRouteEntry],
    ) -> Result<ResolvedModelRoute, HostError> {
        if entries.is_empty() {
            return Err(HostError::InvalidInput(
                "explicit model route must declare at least one candidate".into(),
            ));
        }
        let router = self
            .engine()
            .map(|engine| engine.provider_snapshot().router().clone())
            .unwrap_or_else(|_| (*self.router).clone());
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        let mut candidates = Vec::with_capacity(entries.len());
        let mut selected_index = None;
        for entry in entries {
            let model_str = entry.model.as_str().trim();
            if model_str.is_empty() {
                return Err(HostError::InvalidInput(
                    "model candidate id must not be empty".into(),
                ));
            }
            let requested = match entry.reasoning.as_deref() {
                Some(raw) => Some(ReasoningEffort::parse(raw).ok_or_else(|| {
                    HostError::InvalidInput(format!("invalid reasoning effort `{raw}`"))
                })?),
                None => None,
            };
            let resolved_provider = router.resolve(&entry.model);
            if let Some(effort) = requested
                && effort != ReasoningEffort::Off
                && let Some(ref provider) = resolved_provider
                && provider
                    .capabilities(&entry.model)
                    .is_some_and(|caps| !caps.reasoning_request)
            {
                return Err(HostError::InvalidInput(format!(
                    "model `{}` does not support explicit reasoning effort `{}`",
                    entry.model,
                    effort.as_str()
                )));
            }
            let effective = requested
                .or_else(|| router.reasoning_default(&entry.model))
                .unwrap_or(ReasoningEffort::Off);
            let reasoning = effective.as_str().to_string();
            if !seen.insert((model_str.to_string(), reasoning.clone())) {
                return Err(HostError::InvalidInput(format!(
                    "duplicate model route candidate `{model_str}` with reasoning `{reasoning}`"
                )));
            }
            if selected_index.is_none() && resolved_provider.is_some() {
                selected_index = Some(candidates.len());
            }
            candidates.push(ResolvedModelRouteCandidate {
                model: entry.model.clone(),
                reasoning,
            });
        }
        let Some(selected_index) = selected_index else {
            return Err(HostError::InvalidInput(
                "no candidate in explicit model route is currently servable".into(),
            ));
        };
        Ok(ResolvedModelRoute {
            candidates,
            selected_index,
        })
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
        let Ok(binding) = self.bind_for_workdir(&self.default_agent.workdir).await else {
            return Ok(None);
        };
        Ok(binding
            .bundle_catalog()
            .bundles()
            .iter()
            .find(|bundle| bundle.identity().id == id)
            .map(|bundle| (**bundle).clone()))
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

    async fn resolve_scope_catalog(
        &self,
        session: Option<SessionId>,
        directory: Option<&Path>,
    ) -> Result<ScopeCatalogSnapshot, HostError> {
        let directory = directory.map(Path::to_path_buf);
        self.on_host(async move {
            let engine = self.engine()?;
            let (workdir, binding) = match (session, directory.as_deref()) {
                (Some(session_id), _) => {
                    let workdir = self.session_workdir_strict(session_id).await?;
                    let binding = engine
                        .bind_session_runtime(session_id, &workdir)
                        .await
                        .map_err(map_core_error)?;
                    (workdir, binding)
                }
                (None, Some(dir)) => {
                    let binding = engine
                        .bind_root_runtime(dir)
                        .await
                        .map_err(map_core_error)?;
                    (dir.to_path_buf(), binding)
                }
                (None, None) => {
                    let binding = engine.bind_global_runtime().await.map_err(map_core_error)?;
                    (PathBuf::new(), binding)
                }
            };
            let project_roots = match binding.scope() {
                CatalogScope::Project { roots, .. } => roots.clone(),
                CatalogScope::Directory(dir) => vec![dir.clone()],
                CatalogScope::Global => Vec::new(),
            };
            let bundles = binding
                .bundle_catalog()
                .bundles()
                .iter()
                .map(|bundle| (**bundle).clone())
                .collect();
            let base_fingerprint = engine
                .runtime_semantic_fingerprint_v1(&binding)
                .ok_or_else(|| {
                    HostError::Unavailable("runtime semantic fingerprint is unavailable".into())
                })?;
            let providers = engine.provider_snapshot();
            let mut hasher = Sha256::new();
            hasher.update(base_fingerprint);
            let mut agent_ids: BTreeSet<String> = binding
                .agent_catalog()
                .spawnable(binding.agent_catalog().default_agent_id())
                .unwrap_or_default()
                .into_iter()
                .map(|def| def.stable_id.to_string())
                .collect();
            for bundle in binding.bundle_catalog().bundles() {
                for agent in bundle.agents() {
                    agent_ids.insert(agent.id.as_str().to_string());
                }
            }
            for agent_id in &agent_ids {
                let Some(def) = binding.resolve_agent(agent_id) else {
                    continue;
                };
                if def.model_policy.model.is_none()
                    && def.model_policy.category.is_none()
                    && let Some(model) = binding
                        .agent_model_preference(def.stable_id)
                        .filter(|model| providers.router().resolve(model).is_some())
                {
                    hasher.update(def.stable_id.as_bytes());
                    hasher.update(b"=");
                    hasher.update(model.as_str().as_bytes());
                    hasher.update(b"\0");
                }
            }
            let runtime_fingerprint = hex_digest(&hasher.finalize());
            Ok(ScopeCatalogSnapshot {
                workdir,
                project_roots,
                bundles,
                runtime_fingerprint,
            })
        })
        .await
    }

    async fn authorize_workflow_agent(
        &self,
        session: SessionId,
        caller: &AgentName,
        agent: &AgentName,
    ) -> Result<(), HostError> {
        let caller = caller.clone();
        let agent = agent.clone();
        self.on_host(async move {
            let engine = self.engine()?;
            let workdir = self.session_workdir_strict(session).await?;
            let binding = engine
                .bind_session_runtime(session, &workdir)
                .await
                .map_err(map_core_error)?;
            let _ = self.resolve_managed_member_runtime(
                &engine,
                &binding,
                &workdir,
                Some(&caller),
                &agent,
                None,
            )?;
            Ok(())
        })
        .await
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
        if (kind.is_none() || kind == Some(ExtensionKind::LoopDriver))
            && let Some(drivers) = self.loop_drivers.get()
        {
            for driver in drivers {
                entries.push(ExtensionEntry {
                    bundle_id: "hya/loop".to_string(),
                    kind: ExtensionKind::LoopDriver,
                    name: driver.id().to_string(),
                    metadata: Value::Null,
                });
            }
        }
        Ok(entries)
    }

    async fn has_bundle(&self, bundle_id: &str) -> Result<bool, HostError> {
        if bundle_id == "hya/goal" && self.goal_driver.get().is_some() {
            return Ok(true);
        }
        if bundle_id == "hya/loop"
            && self
                .loop_drivers
                .get()
                .is_some_and(|drivers| !drivers.is_empty())
        {
            return Ok(true);
        }
        Ok(self.get_bundle(bundle_id).await?.is_some())
    }

    async fn invoke_bundle_api(
        &self,
        call: BundleApiInvocation,
    ) -> Result<BundleApiResult, HostError> {
        self.on_host(async move {
            let engine = self.engine()?;
            let outcome = engine
                .invoke_bundle_api(BundleApiCall {
                    bundle: call.bundle,
                    method: call.method,
                    session: call.session,
                    path: call.path,
                    query: call.query,
                    body: call.body,
                })
                .await
                .map_err(|error| match error {
                    BundleApiError::NotFound { .. } | BundleApiError::SessionNotFound(_) => {
                        HostError::NotFound(error.to_string())
                    }
                    BundleApiError::BadRequest(msg) => HostError::InvalidInput(msg),
                    BundleApiError::MethodNotAllowed { .. } => {
                        HostError::InvalidInput(error.to_string())
                    }
                    BundleApiError::Failed { .. } | BundleApiError::Core(_) => {
                        HostError::Internal(error.to_string())
                    }
                })?;
            Ok(BundleApiResult {
                status: outcome.status,
                body: outcome.body,
            })
        })
        .await
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

    async fn invoke_loop_should_stop(
        &self,
        session: SessionId,
        target: &str,
        transcript: &str,
    ) -> Result<Option<String>, HostError> {
        let hooks = self.loop_hooks_for_session(session).await?;
        Ok(hooks.loop_should_stop(target, transcript).await)
    }

    async fn invoke_loop_verify(
        &self,
        session: SessionId,
        target: &str,
        transcript: &str,
    ) -> Result<Option<NativeVerifierVerdict>, HostError> {
        let hooks = self.loop_hooks_for_session(session).await?;
        match hooks.loop_verify(target, transcript).await {
            Ok(verdict) => Ok(Some(NativeVerifierVerdict {
                score: verdict.score,
                satisfied: verdict.satisfied,
                evidence_quality: match verdict.evidence_quality {
                    EvidenceQuality::Missing => "missing",
                    EvidenceQuality::ClaimOnly => "claim_only",
                    EvidenceQuality::Supported => "supported",
                    EvidenceQuality::Verified => "verified",
                }
                .to_string(),
                critical_gaps: verdict.critical_gaps,
                iteration_summary: verdict.iteration_summary,
                reason: verdict.reason,
            })),
            Err(CoreError::Invalid(msg))
                if msg.contains("not registered") || msg.contains("not selected") =>
            {
                Ok(None)
            }
            Err(error) => Err(HostError::Internal(error.to_string())),
        }
    }

    async fn invoke_loop_plan(
        &self,
        session: SessionId,
        input: NativeLoopPlanInput,
    ) -> Result<Option<NativePlannerOutput>, HostError> {
        let hooks = self.loop_hooks_for_session(session).await?;
        let last = VerifierVerdict {
            score: input.last.score,
            satisfied: input.last.satisfied,
            evidence_quality: match input.last.evidence_quality.as_str() {
                "verified" => EvidenceQuality::Verified,
                "supported" => EvidenceQuality::Supported,
                "claim_only" => EvidenceQuality::ClaimOnly,
                _ => EvidenceQuality::Missing,
            },
            critical_gaps: input.last.critical_gaps,
            iteration_summary: input.last.iteration_summary,
            reason: input.last.reason,
        };
        match hooks
            .loop_plan(&input.target, &input.history, &last, &input.planner_notes)
            .await
        {
            Ok(plan) => Ok(Some(NativePlannerOutput {
                directive: plan.directive,
                continuity_brief: plan.continuity_brief,
                planner_notes: plan.planner_notes,
                strategy_change: plan.strategy_change,
                change_note: plan.change_note,
            })),
            Err(CoreError::Invalid(msg))
                if msg.contains("not registered") || msg.contains("not selected") =>
            {
                Ok(None)
            }
            Err(error) => Err(HostError::Internal(error.to_string())),
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

    fn loop_driver(&self) -> Option<Arc<dyn LoopDriver>> {
        self.loop_drivers
            .get()
            .and_then(|drivers| drivers.first().cloned())
    }
}
