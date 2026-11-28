//! App-layer [`hya_runtime_abi::HostRuntime`] implementation injected into
//! ABI v2 native bundles (`hya/coding-extras`, `hya/hashline`, `hya/goal`, `hya/loop`, etc.).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use futures::StreamExt as _;
use hya_bundle::{Bundle, PreparedAgent};
use hya_engine::resident::ActivationResult;
use hya_engine::{
    AgentSpec, BundleApiCall, BundleApiError, CatalogScope, CategoryRegistry, CoreError,
    CreateSession, DrivenTurn, DrivenTurnEnd, ExplicitModelRoute, ExplicitModelRouteCandidate,
    ExplicitTurnRoute, FencedSidecars, HookChain, ResidentSupervisor, SessionEngine,
    SessionStoreExt as _, SidecarEnvironment as _, TurnBinding, apply_agent_model_preference,
    apply_spawn_model_policy,
};
use hya_proto::{
    ActorClaim, AgentName, ChannelPolicySnapshot, Envelope, Event, EventSeq, FinishReason, Message,
    MessageId, ModelRef, OwnerRunId, PartProjection, Projection, Role, SessionId, SessionKind,
    TokenUsage,
};
use hya_provider::{CompletionRequest, ProviderRouter, ReasoningEffort};
use hya_runtime_abi::{
    AgentModelRequest, BundleApiInvocation, BundleApiResult, BundleCatalogSummary,
    BundleClaimLease, BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionKind,
    HostBundleStorage, HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry,
    HostModelCaller, HostRuntime, HostSessionStore, HostTurnRunner, ManagedMemberActivateRequest,
    ManagedMemberActivationOutcome, ManagedMemberActivationStatus, ManagedMemberHandle,
    ManagedMemberSpawnRequest, MemberMailRevivalRequest, ModelCallRequest, ModelCallResponse,
    ModelRouteEntry, ModelStreamChunk, ModelToolCall, ModelWindowInfo, NativeDriver,
    ResolvedModelRoute, ResolvedModelRouteCandidate, ScopeCatalogSnapshot, SessionCloseKind,
    SessionListFilter, SessionSummaryMeta, SessionTreeArchive, TurnOutcome, TurnSpawnRequest,
    TurnStart, TurnStartRequest, TurnStatus,
};
use hya_store::SessionStore;
use hya_tool::{AgentDef, ReportOutcome, SkillCatalogEntry, SpawnMember};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use tokio::sync::{broadcast, mpsc, watch};
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
    /// Drivers of the loaded bundles, by `<bundle-id>:<name>`.
    drivers: std::sync::RwLock<std::collections::BTreeMap<String, Arc<dyn NativeDriver>>>,
    /// Bundle-declared hook points (`<bundle-id>:<point>`) of the loaded bundles.
    declared_hooks: std::sync::RwLock<BTreeSet<String>>,
    /// Sidecars kept across the fenced turns of each session.
    fenced_sidecars: FencedSidecars,
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
            drivers: std::sync::RwLock::new(std::collections::BTreeMap::new()),
            declared_hooks: std::sync::RwLock::new(BTreeSet::new()),
            fenced_sidecars: FencedSidecars::default(),
        }
    }

    /// Record the hook points `bundle_id` declares (local names).
    pub(crate) fn declare_hook_points(&self, bundle_id: &str, points: &[String]) {
        let mut declared = self
            .declared_hooks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        declared.extend(points.iter().map(|point| format!("{bundle_id}:{point}")));
    }

    fn is_declared_hook(&self, point: &str) -> bool {
        self.declared_hooks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(point)
    }

    /// The chain that answers the declared point `point` for `session`.
    async fn declared_hook_chain(
        &self,
        session: Option<SessionId>,
        point: &str,
    ) -> Result<HookChain, HostError> {
        if !self.is_declared_hook(point) {
            return Err(HostError::InvalidInput(format!(
                "no loaded bundle declares hook point `{point}`"
            )));
        }
        let engine = self.engine()?;
        let (workdir, agent) = self.resolve_session_scope(session).await?;
        let binding = match session {
            Some(session) => engine.bind_session_runtime(session, &workdir).await,
            None => engine.bind_root_runtime(&workdir).await,
        }
        .map_err(map_core_error)?;
        Ok(declared_hook_chain(&engine, &binding, &agent, point))
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

    /// Register the drivers `bundle_id` exports as `<bundle-id>:<name>`.
    pub(crate) fn register_drivers(&self, bundle_id: &str, drivers: Vec<Arc<dyn NativeDriver>>) {
        let mut registered = self
            .drivers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for driver in drivers {
            registered.insert(format!("{bundle_id}:{}", driver.name()), driver);
        }
    }

    /// The driver registered as `name` (`<bundle-id>:<name>`).
    pub(crate) fn driver(&self, name: &str) -> Option<Arc<dyn NativeDriver>> {
        self.drivers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
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
                .resolve_agent_run(caller.as_str(), agent.as_str())
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

/// The entries that may answer the declared point `point`: the process-wide
/// chain, user-global plugins, agent-less bundles, the bound agent's hooks,
/// then the declaring bundle's own hooks. When the bound agent belongs to the
/// declaring bundle, the main agent's hooks stand in for its own.
fn declared_hook_chain(
    engine: &SessionEngine,
    binding: &TurnBinding,
    stable_agent_id: &str,
    point: &str,
) -> HookChain {
    let owner = point.split_once(':').map(|(bundle, _)| bundle);
    let active_is_owner = binding
        .resolve_agent(stable_agent_id)
        .is_some_and(|agent| agent.origin.bundle_id() == owner);
    let selected = binding.bundle_hooks_for_agent(if active_is_owner {
        hya_engine::MAIN_AGENT_ID
    } else {
        stable_agent_id
    });
    let agentless = binding
        .bundle_catalog()
        .bundles()
        .iter()
        .filter(|bundle| bundle.agents().is_empty() && Some(bundle.identity().id.as_str()) != owner)
        .filter_map(|bundle| binding.bundle_hooks(&bundle.identity().id))
        .collect::<Vec<_>>();
    let mut entries = engine.process_hooks().entries().to_vec();
    for entry in binding
        .global_plugin_hooks()
        .into_iter()
        .chain(agentless)
        .chain(selected)
        .chain(owner.and_then(|owner| binding.bundle_hooks(owner)))
    {
        if !entries
            .iter()
            .any(|existing| Arc::ptr_eq(existing.hook(), entry.hook()))
        {
            entries.push(entry);
        }
    }
    HookChain::new(entries)
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

    fn stage_extension_event(
        &self,
        during: SessionId,
        target: SessionId,
        bundle: &str,
        kind: &str,
        payload: Value,
        on_commit: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<(), HostError> {
        self.engine()?
            .stage_extension_event(during, target, bundle, kind, payload, on_commit)
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

    async fn append_batch(
        &self,
        fence: Option<ActorClaim>,
        expect: Vec<(SessionId, EventSeq)>,
        events: Vec<(SessionId, Event)>,
    ) -> Result<Vec<Envelope>, HostError> {
        self.on_host(async move {
            self.engine()?
                .host_runtime()
                .session_store()
                .append_batch(fence, expect, events)
                .await
        })
        .await
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

/// The session fence `lease` holds, when its key is a session id.
fn lease_fence(lease: &BundleClaimLease) -> Option<ActorClaim> {
    Some(ActorClaim {
        actor_id: lease.resource_key.parse().ok()?,
        epoch: hya_proto::ActorEpoch::from_storage(lease.epoch),
        owner_run_id: serde_json::from_value(Value::String(lease.owner_id.clone())).ok()?,
    })
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
    fn get_transient_state(
        &self,
        bundle: &str,
        session: SessionId,
        key: &str,
    ) -> Result<Option<Value>, HostError> {
        Ok(self.engine()?.get_transient_state(bundle, session, key))
    }

    fn set_transient_state(
        &self,
        bundle: &str,
        session: SessionId,
        key: &str,
        value: Option<Value>,
    ) -> Result<(), HostError> {
        self.engine()?
            .set_transient_state(bundle, session, key, value);
        Ok(())
    }

    fn subscribe_transient_state(
        &self,
        bundle: &str,
        session: SessionId,
        key: &str,
    ) -> Result<watch::Receiver<Option<Value>>, HostError> {
        Ok(self
            .engine()?
            .subscribe_transient_state(bundle, session, key))
    }

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
        .await?;
        // A released session fence ends the sidecar its fenced turns kept.
        if let Some(fence) = lease_fence(lease) {
            self.fenced_sidecars.release(&fence).await;
        }
        Ok(())
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

    async fn list_child_sessions(&self, parent: SessionId) -> Result<Vec<SessionId>, HostError> {
        self.on_host(async move {
            self.engine()?
                .host_runtime()
                .turn_runner()
                .list_child_sessions(parent)
                .await
        })
        .await
    }

    async fn archive_session_tree(&self, root: SessionId) -> Result<SessionTreeArchive, HostError> {
        self.on_host(async move {
            let engine = self.engine()?;
            engine
                .before_session_close(root, SessionCloseKind::Archive)
                .await
                .map_err(map_core_error)?;
            engine
                .host_runtime()
                .turn_runner()
                .archive_session_tree(root)
                .await
        })
        .await
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
        let session = request.session;
        let meta = self.session_metadata(session).await?;
        let workdir = meta
            .as_ref()
            .and_then(|m| m.workdir.as_deref())
            .filter(|w| !w.is_empty())
            .map_or_else(|| self.default_agent.workdir.clone(), PathBuf::from);
        let agent_name = request
            .agent
            .or_else(|| meta.as_ref().and_then(|m| m.agent.clone()))
            .unwrap_or_else(|| self.default_agent.name.clone());
        let (binding, spec, resolved) = match request.actor_claim {
            // A fenced turn runs as a child of its parent's binding.
            Some(_) => {
                let parent = meta.as_ref().and_then(|m| m.parent).unwrap_or(session);
                let parent_binding = engine
                    .bind_session_runtime(parent, &workdir)
                    .await
                    .map_err(map_core_error)?;
                let (mut spec, (binding, roster, resources, _)) = self
                    .resolve_managed_member_runtime(
                        &engine,
                        &parent_binding,
                        &workdir,
                        None,
                        &agent_name,
                        request.route.as_ref(),
                    )?;
                if let Some(system) = request.system_override {
                    spec.system_prompt = system;
                }
                if let Some(model) = request.model {
                    spec.model = model;
                }
                (binding, spec, Some((roster, resources)))
            }
            None => {
                let binding = engine
                    .bind_session_runtime(session, &workdir)
                    .await
                    .map_err(map_core_error)?;
                let model = request
                    .model
                    .or_else(|| meta.as_ref().and_then(|m| m.model.clone()))
                    .unwrap_or_else(|| self.default_agent.model.clone());
                let spec = AgentSpec {
                    name: agent_name.clone(),
                    model,
                    system_prompt: request
                        .system_override
                        .unwrap_or_else(|| self.default_agent.system_prompt.clone()),
                    workdir,
                    reasoning: self.default_agent.reasoning,
                };
                (binding, spec, None)
            }
        };
        let lease = match request.start {
            TurnStart::Always => None,
            TurnStart::IfIdle => Some(engine.try_begin_turn(session).map_err(|_| {
                HostError::Conflict(format!("session `{session}` has an active turn"))
            })?),
        };
        let admitted_prompt = match request.prompt {
            Some(prompt) => Some(
                engine
                    .admit_user_prompt_with_binding(session, prompt, &binding, agent_name.as_str())
                    .await
                    .map_err(map_core_error)?,
            ),
            None => None,
        };
        let fenced = request.actor_claim.is_some();
        let end = engine
            .run_driven_turn(
                session,
                DrivenTurn {
                    agent: spec,
                    binding,
                    resolved,
                    claim: request.actor_claim,
                    guidance: request.guidance.map(Arc::from),
                    route: request.route.as_ref().map(explicit_route_from_resolved),
                    lease,
                    // A fenced turn answers its prompt however it ends.
                    admitted_prompt: admitted_prompt.filter(|_| fenced),
                },
                &self.fenced_sidecars,
                cancel,
            )
            .await
            .map_err(map_core_error)?;
        if end == DrivenTurnEnd::SidecarLost {
            return Err(HostError::Unavailable("sidecar lost".into()));
        }
        let projection = engine
            .read_projection_shared(session)
            .await
            .map_err(map_core_error)?;
        Ok(extract_turn_outcome(session, &projection))
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
        session: SessionId,
        reason: Option<String>,
    ) -> Result<bool, HostError> {
        self.engine()?
            .host_runtime()
            .turn_runner()
            .cancel_turn(session, reason)
            .await
    }

    async fn turn_status(&self, session: SessionId) -> Result<TurnStatus, HostError> {
        self.engine()?
            .host_runtime()
            .turn_runner()
            .turn_status(session)
            .await
    }

    // Adapter: removed in CHR-47.
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

    // Adapter: removed in CHR-47.
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

    // Adapter: removed in CHR-47.
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

    // Adapter: removed in CHR-47.
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

    // Adapter: removed in CHR-47.
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

    // Adapter: removed in CHR-47.
    async fn session_channel_policy(
        &self,
        session: SessionId,
    ) -> Result<Option<ChannelPolicySnapshot>, HostError> {
        Ok(self
            .engine()
            .ok()
            .and_then(|engine| engine.session_channel_policy(session)))
    }

    // Adapter: removed in CHR-47.
    fn mail_mark(&self, session: SessionId) -> Option<u64> {
        self.engine()
            .ok()
            .and_then(|engine| engine.mail_mark(session))
    }

    // Adapter: removed in CHR-47.
    fn notify_mail_consumed(&self, session: SessionId, mark: u64, consumed_incoming: bool) {
        if let Ok(engine) = self.engine() {
            engine.notify_mail_consumed(session, Some(mark), consumed_incoming);
        }
    }

    // Adapter: removed in CHR-47.
    fn is_member_stopped_by_user(&self, root: SessionId, handle: &str) -> bool {
        self.engine()
            .ok()
            .is_some_and(|engine| engine.is_member_stopped_by_user(root, handle))
    }

    // Adapter: removed in CHR-47.
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

    // Adapter: removed in CHR-47.
    async fn archive_subagent(
        &self,
        child: SessionId,
    ) -> Result<hya_tool::ArchiveReceipt, HostError> {
        self.on_host(async move {
            let engine = self.engine()?;
            engine
                .host_runtime()
                .turn_runner()
                .archive_subagent(child)
                .await
        })
        .await
    }

    // Adapter: removed in CHR-47.
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
            let diagnostics = engine.catalog_diagnostics(binding.scope()).await;
            Ok(ScopeCatalogSnapshot {
                workdir,
                project_roots,
                bundles,
                runtime_fingerprint,
                diagnostics,
            })
        })
        .await
    }

    async fn resolve_agent_model(&self, request: AgentModelRequest) -> Result<ModelRef, HostError> {
        self.on_host(async move {
            let engine = self.engine()?;
            let workdir = self.session_workdir_strict(request.parent).await?;
            let binding = engine
                .bind_session_runtime(request.parent, &workdir)
                .await
                .map_err(map_core_error)?;
            let caller = self
                .read_projection(request.parent)
                .await?
                .session
                .agent
                .clone()
                .unwrap_or_else(|| self.default_agent.name.clone());
            let allowed = engine
                .agent_roster_for_binding(&binding, caller.as_str())
                .map_err(map_core_error)?;
            let environment = engine.sidecar_environment().ok_or_else(|| {
                HostError::Unavailable("sidecar environment is not attached".into())
            })?;
            let mut base = self.default_agent.clone();
            base.workdir = workdir;
            let providers = engine.provider_snapshot();
            let is_servable = |model: &ModelRef| providers.router().resolve(model).is_some();
            let resolved = hya_engine::resident::resolve_spawn_member(
                &hya_engine::resident::ResolveSpawnMemberCtx {
                    engine: &engine,
                    binding: &binding,
                    base: &base,
                    caller: caller.as_str(),
                    allowed_agents: &allowed,
                    categories: &self.categories,
                    is_servable: &is_servable,
                    guidance: None,
                    sidecar_environment: environment.as_ref(),
                },
                SpawnMember {
                    description: String::new(),
                    prompt: String::new(),
                    subagent_type: request.agent.as_str().to_string(),
                    model: request.model.map(|model| model.to_string()),
                    category: request.category,
                    inline_agent: None,
                    effort: request.effort,
                },
            )
            .map_err(|error| HostError::InvalidInput(error.to_string()))?;
            Ok(resolved.agent.model)
        })
        .await
    }

    async fn authorize_agent_run(
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
        if kind.is_none() || kind == Some(ExtensionKind::Driver) {
            let drivers = self
                .drivers
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for name in drivers.keys() {
                let (bundle_id, local) = name.split_once(':').unwrap_or((name.as_str(), ""));
                entries.push(ExtensionEntry {
                    bundle_id: bundle_id.to_string(),
                    kind: ExtensionKind::Driver,
                    name: local.to_string(),
                    metadata: Value::Null,
                });
            }
        }
        Ok(entries)
    }

    async fn has_bundle(&self, bundle_id: &str) -> Result<bool, HostError> {
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

    async fn has_hook(&self, session: Option<SessionId>, point: &str) -> Result<bool, HostError> {
        Ok(self
            .declared_hook_chain(session, point)
            .await?
            .declares(point))
    }

    async fn invoke_hook(
        &self,
        session: Option<SessionId>,
        point: &str,
        input: Value,
    ) -> Result<Option<Value>, HostError> {
        Ok(self
            .declared_hook_chain(session, point)
            .await?
            .invoke_declared(point, input)
            .await)
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
}
