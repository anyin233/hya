use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::{
    AgentName, Envelope, Event, EventSeq, MessageId, ModelRef, OperationId, Projection, Role,
    SessionId, ToolCallId, ToolSchema, now_millis,
};
use hya_provider::{ProviderCatalogSnapshot, ProviderModel, ProviderRouter, ReasoningEffort};
use hya_store::{ActorClaim, SessionStore};
use hya_tool::handle::{ArtifactHook, ArtifactPlane};
use hya_tool::{
    AgentDef, FormatterPlane, InteractionPlane, LifecyclePlane, LspPlane, MailboxPlane,
    PermissionPlane, PermissionRules, ResolvedTool, SpawnRequest, SpawnRequestSendError,
    SpawnRequestSink, SpawnerPlane, TodoPlane, ToolError, WebSearchPlane,
};
use serde_json::Value;

#[cfg(test)]
use tokio::sync::Notify;

use crate::agent_catalog::AgentDefinition;
use crate::bus::EventBus;
use crate::compaction::{CompactionConfig, SummarizeOptions, Summarizer};
use crate::error::CoreError;
use crate::hooks::{HookChain, HookDispatcher, SessionLifecycleInput, activation_hook_for};
use crate::runtime_registry::CompiledResourceView;
use crate::sidecar::SidecarEnvironment;
use crate::tokens::TokenAccounting;
use crate::{
    AgentResourcePolicy, CategoryRegistry, RuntimeCandidate, RuntimeRefreshError, RuntimeRegistry,
    TurnBinding,
};

/// Closed set of fixed Harness system-operation agents.
///
/// Exact catalog lookup only — not spawn, not roster, and not an arbitrary-ID
/// bypass. Callers cannot pass an open string; only these three operations exist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixedSystemAgent {
    Compaction,
    Title,
    Summary,
}

impl FixedSystemAgent {
    /// Stable catalog id for this fixed system operation.
    const fn stable_id(self) -> &'static str {
        match self {
            Self::Compaction => "compaction",
            Self::Title => "title",
            Self::Summary => "summary",
        }
    }
}

mod admission;
mod file_snapshot;
mod fork;
mod handoff;
mod mailbox;
pub(crate) use mailbox::dm_channel_between;
mod members;
pub(crate) use members::MemberSpawnRecord;
mod model_probe;
mod revert;
pub use model_probe::{MODEL_PROBE_PROMPT, ModelProbeReply};
mod session_cleanup;
mod session_state;
mod session_title;
mod shell;
mod spill;
mod steer;
mod stream_round;
mod summary;
mod text_complete;
mod todos;
mod tool_error;
mod turn;
mod turn_end;
mod turn_gate;
pub(crate) use turn::TurnRequestContext;

async fn authorize_tool_call(
    resolved: &ResolvedTool,
    input: &Value,
    permission: PermissionPlane,
    message: MessageId,
    call: ToolCallId,
) -> Result<PermissionPlane, ToolError> {
    let invocation = resolved.invocation(input)?;
    permission
        .for_tool_call(message, call)
        .authorize(&invocation)
        .await
        .map_err(ToolError::from)
}

pub use admission::SpawnAdmissionOutcome;
pub use file_snapshot::{MAX_DIRTY_BYTES, MAX_DIRTY_FILES, MAX_FILE_BYTES, MAX_SESSION_BLOB_BYTES};
pub use fork::{ForkAt, ForkError, fork_cut};
pub use revert::{RevertError, RevertOutcome, RevertTarget};
pub use turn::advertise_tool;
pub use turn_end::{DRAIN_DEADLINE, TurnDrainReport};
pub use turn_gate::{TurnBoundaryObserver, TurnLease};

/// Parameters for creating a new session event log.
pub struct CreateSession {
    /// Parent session for subagents; `None` for a root/interactive session.
    pub parent: Option<SessionId>,
    /// Agent name recorded on `SessionCreated`.
    pub agent: AgentName,
    /// Initial model for the session.
    pub model: ModelRef,
    /// Working directory string stored on the session.
    pub workdir: String,
}

/// Turn-time agent identity: name, model, prompt, workdir, and reasoning effort.
#[derive(Clone)]
pub struct AgentSpec {
    /// Agent display / catalog name.
    pub name: AgentName,
    /// Model route for completions.
    pub model: ModelRef,
    /// System prompt base before guidance/skills composition.
    pub system_prompt: String,
    /// Filesystem workdir for tools and path resolution.
    pub workdir: PathBuf,
    /// Optional reasoning effort for capable models.
    pub reasoning: Option<ReasoningEffort>,
}

/// Process-local identity for one admitted member in a parent orchestration turn.
///
/// This value is only an in-process binding between admission and nested spawn
/// observation. It is never persisted, serialized onto the wire, or used as
/// session or event authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionMemberIdentity {
    /// Operation id of the parent tool call that admitted the member.
    pub operation_id: OperationId,
    /// Zero-based ordinal within that admission batch.
    pub member_ordinal: u32,
}

tokio::task_local! {
    static CURRENT_ADMISSION_MEMBER: Option<AdmissionMemberIdentity>;
}

pub(crate) async fn scope_admission_member<F, T>(
    admission: Option<AdmissionMemberIdentity>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    CURRENT_ADMISSION_MEMBER.scope(admission, future).await
}

pub(crate) fn current_admission_member() -> Option<AdmissionMemberIdentity> {
    CURRENT_ADMISSION_MEMBER
        .try_with(|admission| *admission)
        .ok()
        .flatten()
}

#[cfg(test)]
pub(crate) struct DirectMailPreAppendGate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[cfg(test)]
impl DirectMailPreAppendGate {
    pub(crate) fn new(entered: Arc<Notify>, release: Arc<Notify>) -> Self {
        Self { entered, release }
    }
}

/// Optional app-owned hook to refresh the runtime catalog before a root bind.
///
/// **Contract:** Called from [`SessionEngine::bind_root_runtime`]. Return
/// `Ok(true)` when a new generation was published, `Ok(false)` when nothing
/// changed. Errors abort the bind. Implementors own MCP/plugin discovery I/O;
/// the engine only rebinds after a successful refresh.
#[async_trait]
pub trait RuntimeCatalogRefresh: Send + Sync {
    /// Refresh `runtime` if external sources changed.
    ///
    /// # Errors
    /// Propagate discovery or publication failures as [`CoreError`].
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError>;
}

/// One spawn request bound to the immutable runtime snapshot of its parent turn.
///
/// This value is process-local orchestration state. It is never persisted or
/// exposed on the wire; dropping it naturally releases the retained snapshot.
pub struct BoundSpawnRequest {
    binding: TurnBinding,
    request: SpawnRequest,
    admission: Option<AdmissionMemberIdentity>,
}

impl BoundSpawnRequest {
    /// Return the process-local identity of the admitted parent member, if any.
    #[must_use]
    pub fn parent_admission(&self) -> Option<AdmissionMemberIdentity> {
        self.admission
    }

    /// Consume into the retained turn binding and the raw tool-plane spawn request.
    #[must_use]
    pub fn into_parts(self) -> (TurnBinding, SpawnRequest) {
        (self.binding, self.request)
    }
}

/// Core-owned sender for parent-turn-bound spawn requests.
#[derive(Clone)]
pub struct BoundSpawnSender {
    tx: tokio::sync::mpsc::Sender<BoundSpawnRequest>,
}

impl BoundSpawnSender {
    /// Create a bounded channel pair for the app's spawn worker loop.
    #[must_use]
    pub fn with_capacity(
        capacity: usize,
    ) -> (Self, tokio::sync::mpsc::Receiver<BoundSpawnRequest>) {
        let capacity = capacity.clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    fn disconnected() -> Self {
        let (sender, receiver) = Self::with_capacity(1);
        drop(receiver);
        sender
    }

    /// Derive a raw tool-plane facade closed over one already-admitted turn.
    #[must_use]
    pub fn for_binding(&self, binding: &TurnBinding) -> SpawnerPlane {
        SpawnerPlane::from_sink(Arc::new(BoundSpawnRequestSink {
            tx: self.tx.clone(),
            binding: binding.clone(),
        }))
    }
}

struct BoundSpawnRequestSink {
    tx: tokio::sync::mpsc::Sender<BoundSpawnRequest>,
    binding: TurnBinding,
}

impl SpawnRequestSink for BoundSpawnRequestSink {
    fn try_send(&self, request: SpawnRequest) -> Result<(), SpawnRequestSendError> {
        self.tx
            .try_send(BoundSpawnRequest {
                binding: self.binding.clone(),
                request,
                admission: current_admission_member(),
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => SpawnRequestSendError::Full,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => SpawnRequestSendError::Closed,
            })
    }
}

/// A workflow-tool request bound to the triggering turn's runtime snapshot.
pub struct BoundWorkflowRequest {
    binding: TurnBinding,
    request: hya_tool::WorkflowRequest,
}

impl BoundWorkflowRequest {
    /// Consume into the retained turn binding and the raw tool-plane request.
    #[must_use]
    pub fn into_parts(self) -> (TurnBinding, hya_tool::WorkflowRequest) {
        (self.binding, self.request)
    }
}

/// Core-owned sender for turn-bound workflow requests.
#[derive(Clone)]
pub struct BoundWorkflowSender {
    tx: tokio::sync::mpsc::Sender<BoundWorkflowRequest>,
}

impl BoundWorkflowSender {
    /// Create a bounded channel pair for the host's workflow worker loop.
    #[must_use]
    pub fn with_capacity(
        capacity: usize,
    ) -> (Self, tokio::sync::mpsc::Receiver<BoundWorkflowRequest>) {
        let capacity = capacity.clamp(1, 65_535);
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    fn disconnected() -> Self {
        let (sender, receiver) = Self::with_capacity(1);
        drop(receiver);
        sender
    }

    /// Derive a raw tool-plane facade closed over one already-resolved turn.
    #[must_use]
    pub fn for_binding(&self, binding: &TurnBinding) -> hya_tool::WorkflowPlane {
        hya_tool::WorkflowPlane::from_sink(Arc::new(BoundWorkflowSink {
            tx: self.tx.clone(),
            binding: binding.clone(),
        }))
    }
}

struct BoundWorkflowSink {
    tx: tokio::sync::mpsc::Sender<BoundWorkflowRequest>,
    binding: TurnBinding,
}

impl hya_tool::WorkflowRequestSink for BoundWorkflowSink {
    fn try_send(
        &self,
        request: hya_tool::WorkflowRequest,
    ) -> Result<(), hya_tool::WorkflowSendError> {
        self.tx
            .try_send(BoundWorkflowRequest {
                binding: self.binding.clone(),
                request,
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    hya_tool::WorkflowSendError::Full
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    hya_tool::WorkflowSendError::Closed
                }
            })
    }
}

/// Central session runtime: persistence, providers, tools, and turn execution.
///
/// Construct with [`SessionEngine::new`], then chain `with_*` builders for
/// interaction, spawn, mailbox, hooks, compaction, and governors. Turns append
/// events to the store and publish on the bus; observers never write the log.
///
/// Clone is cheap: every field is a handle or shared state, so detached tasks
/// (backgrounded MCP watchers) can carry their own engine handle.
pub struct SessionEngine {
    store: SessionStore,
    providers: RwLock<Arc<ProviderRouter>>,
    catalog: RwLock<Arc<ProviderCatalogSnapshot>>,
    /// Cross-model failover plane: preferred [`ModelRef`] → its ordered
    /// candidate chain (the preferred model itself first). Empty by default,
    /// which keeps turn streaming byte-identical to a direct router call.
    model_fallbacks: HashMap<ModelRef, Vec<ModelRef>>,
    /// Configured Agent model categories used by fixed system-Agent calls.
    model_categories: Arc<CategoryRegistry>,
    runtime: Arc<RuntimeRegistry>,
    catalog_refresh: Option<Arc<dyn RuntimeCatalogRefresh>>,
    permission: PermissionPlane,
    interaction: InteractionPlane,
    spawner: BoundSpawnSender,
    workflows: BoundWorkflowSender,
    mailbox: MailboxPlane,
    lifecycle: LifecyclePlane,
    /// Mints subagent handle leaves (`<prefix>-<operator>`); injectable RNG.
    handle_namer: Arc<crate::handle_naming::HandleNamer>,
    todo: TodoPlane,
    websearch: WebSearchPlane,
    /// User-registered `artifact://` post-processing, carried to every tool call.
    artifacts: ArtifactPlane,
    formatter: FormatterPlane,
    lsp: LspPlane,
    bus: EventBus,
    summarizer: Option<Arc<dyn Summarizer>>,
    compaction: CompactionConfig,
    token_accounting: TokenAccounting,
    /// Family tokenizers backing the usage-ledger fallback estimate.
    usage_tokenizers: Arc<crate::model_tokenizers::ModelTokenizerSource>,
    hooks: Option<Arc<dyn HookDispatcher>>,
    session_bundle_hooks: Arc<RwLock<HashMap<SessionId, Arc<dyn HookDispatcher>>>>,
    session_channel_policies: Arc<RwLock<HashMap<SessionId, hya_tool::ChannelPolicySnapshot>>>,
    governor: Option<crate::orchestrator::SubagentGovernor>,
    sidecar_environment: Option<Arc<dyn SidecarEnvironment>>,
    /// Revival seam (ADR-0015): a downward mail to an archived direct child
    /// routes here instead of failing the send. Interior-mutable because the
    /// implementor (resident supervisor) itself holds the engine —
    /// [`ResidentSupervisor::start`](crate::resident::ResidentSupervisor::start)
    /// wires it.
    reviver: RwLock<Option<Arc<dyn ArchiveReviver>>>,
    /// Foreground budget for `mcp__` tool calls; `None` (default) keeps every
    /// call synchronous.
    mcp_background_after: Option<Duration>,
    /// Monotonic `mcpbg-N` job ids for backgrounded MCP calls.
    background_job_seq: Arc<AtomicU64>,
    /// Single-active-turn registry shared by every clone of this engine.
    turn_gate: Arc<turn_gate::TurnGate>,
    #[cfg(test)]
    direct_mail_pre_append_gate: Option<Arc<DirectMailPreAppendGate>>,
}

impl Clone for SessionEngine {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            providers: RwLock::new(
                self.providers
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            ),
            catalog: RwLock::new(
                self.catalog
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            ),
            model_fallbacks: self.model_fallbacks.clone(),
            model_categories: self.model_categories.clone(),
            runtime: self.runtime.clone(),
            catalog_refresh: self.catalog_refresh.clone(),
            permission: self.permission.clone(),
            interaction: self.interaction.clone(),
            spawner: self.spawner.clone(),
            workflows: self.workflows.clone(),
            mailbox: self.mailbox.clone(),
            lifecycle: self.lifecycle.clone(),
            handle_namer: self.handle_namer.clone(),
            todo: self.todo.clone(),
            websearch: self.websearch.clone(),
            artifacts: self.artifacts.clone(),
            formatter: self.formatter.clone(),
            lsp: self.lsp.clone(),
            bus: self.bus.clone(),
            summarizer: self.summarizer.clone(),
            compaction: self.compaction,
            token_accounting: self.token_accounting.clone(),
            usage_tokenizers: self.usage_tokenizers.clone(),
            hooks: self.hooks.clone(),
            session_bundle_hooks: Arc::clone(&self.session_bundle_hooks),
            session_channel_policies: Arc::clone(&self.session_channel_policies),
            governor: self.governor.clone(),
            sidecar_environment: self.sidecar_environment.clone(),
            reviver: RwLock::new(
                self.reviver
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
            ),
            mcp_background_after: self.mcp_background_after,
            background_job_seq: self.background_job_seq.clone(),
            turn_gate: Arc::clone(&self.turn_gate),
            #[cfg(test)]
            direct_mail_pre_append_gate: self.direct_mail_pre_append_gate.clone(),
        }
    }
}

/// Revival seam for archived direct children (ADR-0015).
///
/// The mailbox write gate calls this when a direct-handle send resolves to an
/// archived agent that is the **sender's own direct child**; everything else
/// stays an indistinguishable rejection. The implementation (resident
/// supervisor) re-registers the agent under a bumped claim epoch, seeds the
/// new episode from the latest handoff, and arms it with `body` as the wake
/// prompt.
#[async_trait::async_trait]
pub trait ArchiveReviver: Send + Sync {
    /// Revive `child_handle` on team `root` for parent handle `parent`.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] when the handle is not an archived
    /// direct child of `parent`, or registration cannot be re-resolved.
    async fn revive(
        &self,
        root: SessionId,
        parent: &str,
        child_handle: &str,
        body: String,
    ) -> Result<(), CoreError>;

    /// Root-turn teardown (ADR-0015): release every live actor claim under
    /// `root` and stop the resident tasks. Event emission happens engine-side
    /// before this is called.
    async fn teardown_root(&self, root: SessionId) -> Result<(), CoreError>;
}

impl SessionEngine {
    /// Build an engine with disconnected mailbox/spawner and default tool planes.
    ///
    /// Wire product planes with `with_*` before serving interactive traffic.
    #[must_use]
    pub fn new(
        store: SessionStore,
        providers: Arc<ProviderRouter>,
        runtime: Arc<RuntimeRegistry>,
        permission: PermissionPlane,
        bus: EventBus,
    ) -> Self {
        let (interaction, _rx) = InteractionPlane::new();
        let spawner = BoundSpawnSender::disconnected();
        let workflows = BoundWorkflowSender::disconnected();
        let mailbox = MailboxPlane::disconnected();
        let lifecycle = LifecyclePlane::disconnected();
        let todo = TodoPlane::default();
        let websearch = WebSearchPlane::default();
        let formatter = FormatterPlane::default();
        let lsp = LspPlane::default();
        let catalog = providers.catalog_snapshot_arc().unwrap_or_else(|| {
            Arc::new(ProviderCatalogSnapshot::build(
                providers.catalog(),
                Vec::new(),
                None,
            ))
        });
        Self {
            store,
            providers: RwLock::new(providers),
            catalog: RwLock::new(catalog),
            model_fallbacks: HashMap::new(),
            model_categories: Arc::new(CategoryRegistry::default()),
            runtime,
            catalog_refresh: None,
            permission,
            interaction,
            spawner,
            workflows,
            mailbox,
            lifecycle,
            handle_namer: Arc::new(crate::handle_naming::HandleNamer::default()),
            todo,
            websearch,
            artifacts: ArtifactPlane::default(),
            formatter,
            lsp,
            bus,
            summarizer: None,
            compaction: CompactionConfig::default(),
            token_accounting: TokenAccounting::default(),
            usage_tokenizers: Arc::new(crate::model_tokenizers::ModelTokenizerSource::default()),
            hooks: None,
            session_bundle_hooks: Arc::new(RwLock::new(HashMap::new())),
            session_channel_policies: Arc::new(RwLock::new(HashMap::new())),
            governor: None,
            sidecar_environment: None,
            reviver: RwLock::new(None),
            mcp_background_after: None,
            background_job_seq: Arc::new(AtomicU64::new(1)),
            turn_gate: Arc::new(turn_gate::TurnGate::default()),
            #[cfg(test)]
            direct_mail_pre_append_gate: None,
        }
    }

    /// Move `mcp__`-namespaced tool calls still running after `budget` to the
    /// background: the turn receives an early "backgrounded" tool result and
    /// the real result is later delivered as a steered user prompt. Disabled
    /// (the default) when unset.
    #[must_use]
    pub fn with_mcp_background_after(mut self, budget: Duration) -> Self {
        self.mcp_background_after = Some(budget);
        self
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_direct_mail_pre_append_gate(
        mut self,
        gate: DirectMailPreAppendGate,
    ) -> Self {
        self.direct_mail_pre_append_gate = Some(Arc::new(gate));
        self
    }

    /// Install the cross-model failover plane: preferred [`ModelRef`] → its
    /// ordered candidate chain (the preferred model itself first, matching
    /// `ResolvedCategory.fallback_chain` semantics).
    ///
    /// Chains whose first candidate is not the preferred model are ignored
    /// rather than half-honored. With the default empty plane every request
    /// streams through its single preferred model exactly as before.
    #[must_use]
    pub fn with_model_fallbacks(mut self, fallbacks: HashMap<ModelRef, Vec<ModelRef>>) -> Self {
        self.model_fallbacks = fallbacks
            .into_iter()
            .filter(|(preferred, chain)| chain.first() == Some(preferred))
            .collect();
        self
    }

    /// Install the configured Agent model-category registry.
    #[must_use]
    pub fn with_model_categories(mut self, categories: Arc<CategoryRegistry>) -> Self {
        self.model_categories = categories;
        self
    }

    /// Ordered cross-model candidates for a preferred model (preferred
    /// first), or `None` when no chain is configured for it.
    pub(crate) fn model_fallback_chain(&self, preferred: &ModelRef) -> Option<&[ModelRef]> {
        Some(self.model_fallbacks.get(preferred)?.as_slice())
    }

    /// Install plugin/host hooks for command/tool/chat interception and events.
    #[must_use]
    pub fn with_hooks(mut self, hooks: Arc<dyn HookDispatcher>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Install the environment used to start Bundle sidecars for public packages.
    #[must_use]
    pub fn with_sidecar_environment(mut self, environment: Arc<dyn SidecarEnvironment>) -> Self {
        self.sidecar_environment = Some(environment);
        self
    }

    /// Environment used to resolve per-agent bound sidecar factories, mirroring
    /// the task-tool spawn path; `None` outside application-owned engines.
    #[must_use]
    pub fn sidecar_environment(&self) -> Option<Arc<dyn SidecarEnvironment>> {
        self.sidecar_environment.clone()
    }

    /// Install optional catalog refresh before root runtime binds.
    #[must_use]
    pub fn with_catalog_refresh(mut self, refresh: Arc<dyn RuntimeCatalogRefresh>) -> Self {
        self.catalog_refresh = Some(refresh);
        self
    }

    /// Replace the default disconnected interaction plane.
    #[must_use]
    pub fn with_interaction(mut self, interaction: InteractionPlane) -> Self {
        self.interaction = interaction;
        self
    }

    /// Install the bound spawn sender used by the `task` tool plane.
    #[must_use]
    pub fn with_spawn_sender(mut self, spawner: BoundSpawnSender) -> Self {
        self.spawner = spawner;
        self
    }

    /// Install the host-side receiver pair that serves `workflow` tool runs.
    ///
    /// Mirrors [`Self::with_spawn_sender`]: the engine owns this sender for its
    /// whole life, and the host worker owns an `Arc<SessionEngine>` while it
    /// drains, so queued requests always find a live executor.
    #[must_use]
    pub fn with_workflow_sender(mut self, workflows: BoundWorkflowSender) -> Self {
        self.workflows = workflows;
        self
    }

    /// Inject the mailbox plane whose service loop this engine drives (see
    /// [`run_mailbox_service`](crate::mailbox::run_mailbox_service)). Wired from
    /// the app layer alongside the spawner, mirroring the established plane
    /// pattern so `hya-tool` stays free of a `hya-core` dependency.
    #[must_use]
    pub fn with_mailbox(mut self, mailbox: MailboxPlane) -> Self {
        self.mailbox = mailbox;
        self
    }

    /// Install the lifecycle plane drained by
    /// [`run_lifecycle_service`](crate::run_lifecycle_service).
    #[must_use]
    pub fn with_lifecycle(mut self, lifecycle: LifecyclePlane) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    /// Replace the random source of subagent handle names (tests seed or
    /// force it; see [`crate::handle_naming`]). The default draws from process
    /// entropy, or from the `HYA_HANDLE_SEED` seed when that is set.
    #[must_use]
    pub fn with_handle_rng(self, rng: Box<dyn crate::handle_naming::HandleRng>) -> Self {
        self.handle_namer.set_rng(rng);
        self
    }

    /// Mint a fresh handle leaf `<prefix>-<operator>` for a new member of
    /// `root`'s team: never the leaf of any handle the team ever registered
    /// (live or archived) nor one this process already minted for it.
    pub(crate) async fn mint_member_leaf(&self, root: SessionId, prefix: &str) -> String {
        let taken = self
            .read_projection_shared(root)
            .await
            .map(|projection| crate::handle_naming::team_leaves(&projection))
            .unwrap_or_default();
        self.handle_namer.mint(root, prefix, taken)
    }

    /// Install the archive-revival seam (ADR-0015). Without it, mail to an
    /// archived handle stays an ordinary rejection.
    #[must_use]
    pub fn with_reviver(self, reviver: Arc<dyn ArchiveReviver>) -> Self {
        self.set_reviver(reviver);
        self
    }

    /// Wire the archive-revival seam on a shared engine. Idempotent; the
    /// resident supervisor calls this from its own start.
    pub fn set_reviver(&self, reviver: Arc<dyn ArchiveReviver>) {
        match self.reviver.write() {
            Ok(mut slot) => *slot = Some(reviver),
            Err(poisoned) => *poisoned.into_inner() = Some(reviver),
        }
    }

    /// Claim `session`'s single active turn without waiting.
    ///
    /// A session runs at most one turn at a time; every turn path holds this
    /// lease for the whole turn. Dropping the lease releases the claim.
    ///
    /// # Errors
    /// [`CoreError::TurnAlreadyActive`] when the session already has a turn.
    pub fn try_begin_turn(&self, session: SessionId) -> Result<TurnLease, CoreError> {
        self.turn_gate.try_acquire(session)
    }

    /// Whether `session` currently has an active turn.
    #[must_use]
    pub fn turn_active(&self, session: SessionId) -> bool {
        self.turn_gate.is_active(session)
    }

    /// Install the turn-boundary observer (the resident supervisor). Held
    /// weakly so the observer may own the engine.
    pub fn set_turn_observer(&self, observer: std::sync::Weak<dyn TurnBoundaryObserver>) {
        self.turn_gate.set_observer(observer);
    }

    /// Claim `session`'s turn, queueing behind an active one. `Ok(None)` when
    /// `cancel` fires while waiting.
    pub(crate) async fn begin_turn(
        &self,
        session: SessionId,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Option<TurnLease>, CoreError> {
        self.turn_gate.acquire(session, cancel).await
    }

    /// The installed archive reviver, if any.
    #[must_use]
    pub fn archive_reviver(&self) -> Option<Arc<dyn ArchiveReviver>> {
        match self.reviver.read() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Install the `SubagentGovernor` that bounds nested/parallel subagent
    /// streaming concurrency and per-run budget.
    #[must_use]
    pub fn with_governor(mut self, governor: crate::orchestrator::SubagentGovernor) -> Self {
        self.governor = Some(governor);
        self
    }

    /// Borrow the installed subagent governor, if any.
    #[must_use]
    pub fn governor(&self) -> Option<&crate::orchestrator::SubagentGovernor> {
        self.governor.as_ref()
    }

    /// Replace the default disconnected LSP plane.
    #[must_use]
    pub fn with_lsp(mut self, lsp: LspPlane) -> Self {
        self.lsp = lsp;
        self
    }

    /// Replace the default disconnected formatter plane.
    #[must_use]
    pub fn with_formatter(mut self, formatter: FormatterPlane) -> Self {
        self.formatter = formatter;
        self
    }

    /// Replace the default web-search plane configuration.
    #[must_use]
    pub fn with_websearch(mut self, websearch: WebSearchPlane) -> Self {
        self.websearch = websearch;
        self
    }

    /// Register the `artifact://` post-processing chain.
    ///
    /// Hooks run when an artifact is *retrieved*, never when it is written, so
    /// the captured bytes stay authoritative and a hook that turns out to be
    /// wrong has not already destroyed the output it was summarizing. They are
    /// applied in the order given, which is what lets "strip build noise" and
    /// "extract the failing assertion" compose into one retrieval.
    #[must_use]
    pub fn with_artifact_hooks(mut self, hooks: Vec<Arc<dyn ArtifactHook>>) -> Self {
        self.artifacts = ArtifactPlane::new(hooks);
        self
    }

    /// Enable compaction with a summarizer implementation and thresholds.
    #[must_use]
    pub fn with_compaction(
        mut self,
        summarizer: Arc<dyn Summarizer>,
        config: CompactionConfig,
    ) -> Self {
        self.summarizer = Some(summarizer);
        self.compaction = config;
        self
    }

    /// Set compaction thresholds without wiring a summarizer.
    ///
    /// Legitimate since the model-free rungs landed: a ladder ordered onto
    /// `shake`/`snapcompact` folds with no model call at all, so an engine can
    /// compact without a summarizer and the summarizer-backed rungs simply
    /// advance past.
    #[must_use]
    pub fn with_compaction_config(mut self, config: CompactionConfig) -> Self {
        self.compaction = config;
        self
    }

    /// Choose how window occupancy is measured for compaction decisions.
    ///
    /// Defaults to [`crate::tokens::TokenAccountingMode::Auto`], which believes
    /// a route's reported usage only while it stays plausible and otherwise
    /// falls back to the local tokenizer.
    #[must_use]
    pub fn with_token_accounting(mut self, accounting: TokenAccounting) -> Self {
        self.token_accounting = accounting;
        self
    }

    /// Override the family-tokenizer source backing the usage-ledger fallback
    /// estimate (tests inject fixture tokenizers so CI never downloads).
    #[must_use]
    pub fn with_usage_tokenizers(
        mut self,
        source: Arc<crate::model_tokenizers::ModelTokenizerSource>,
    ) -> Self {
        self.usage_tokenizers = source;
        self
    }

    /// Live event bus for this engine.
    #[must_use]
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// Session event store.
    #[must_use]
    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// LSP plane used by tools and post-edit hooks.
    #[must_use]
    pub fn lsp(&self) -> &LspPlane {
        &self.lsp
    }

    /// Snapshot of resource permission rules currently active on the plane.
    #[must_use]
    pub fn permission_rules(&self) -> PermissionRules {
        self.permission.snapshot_rules()
    }

    /// Formatter plane used after write/edit/patch.
    #[must_use]
    pub fn formatter(&self) -> &FormatterPlane {
        &self.formatter
    }

    /// Provider catalog models exposed to the UI/API.
    #[must_use]
    pub fn provider_catalog(&self) -> Vec<ProviderModel> {
        self.provider_catalog_snapshot().models().to_vec()
    }

    /// Shared provider catalog snapshot including declared provider statuses.
    #[must_use]
    pub fn provider_catalog_snapshot(&self) -> Arc<ProviderCatalogSnapshot> {
        match self.catalog.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// Shared provider router retained by this engine for request-local Workflow routes.
    #[must_use]
    pub fn provider_router(&self) -> Arc<ProviderRouter> {
        match self.providers.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// Atomically replace live provider routes and the presentation catalog.
    ///
    /// Used after background `models.yml.cache` refresh so HTTP/TUI surfaces
    /// observe the new rows without restarting the process.
    pub fn publish_provider_catalog(
        &self,
        providers: Arc<ProviderRouter>,
        catalog: Arc<ProviderCatalogSnapshot>,
    ) {
        match self.providers.write() {
            Ok(mut guard) => *guard = providers,
            Err(poisoned) => *poisoned.into_inner() = providers,
        }
        match self.catalog.write() {
            Ok(mut guard) => *guard = catalog,
            Err(poisoned) => *poisoned.into_inner() = catalog,
        }
    }

    /// Tool schemas from the current effective runtime snapshot.
    #[must_use]
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.runtime.tool_schemas()
    }

    /// Shared runtime registry handle.
    #[must_use]
    pub fn runtime_registry(&self) -> Arc<RuntimeRegistry> {
        self.runtime.clone()
    }

    /// Semantic fingerprint for a bound turn (tools + permissions + sources).
    #[must_use]
    pub fn runtime_semantic_fingerprint_v1(&self, binding: &TurnBinding) -> Option<[u8; 32]> {
        binding.semantic_fingerprint_v1(&self.permission)
    }

    /// Bind a turn against the current registry without refreshing catalogs.
    ///
    /// # Errors
    /// Returns [`CoreError::RuntimeRefresh`] when binding fails.
    pub fn bind_runtime(&self, workdir: &std::path::Path) -> Result<TurnBinding, CoreError> {
        Ok(self.runtime.bind_turn(workdir)?)
    }

    /// Optionally refresh external catalogs, then bind a root turn for `workdir`.
    ///
    /// # Errors
    /// Propagates catalog refresh or bind failures.
    pub async fn bind_root_runtime(
        &self,
        workdir: &std::path::Path,
    ) -> Result<TurnBinding, CoreError> {
        if let Some(refresh) = &self.catalog_refresh {
            let _ = refresh.refresh_if_changed(self.runtime.as_ref()).await?;
        }
        Ok(self.runtime.bind_turn(workdir)?)
    }

    /// Bind a fresh runtime for a Session and apply its root-tree model
    /// overrides. Catalog refresh and skill discovery happen exactly as for a
    /// root bind; temporary models are filtered against currently available
    /// providers without mutating their durable projection.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] when `session` (or its lineage root) is
    /// absent, plus runtime refresh/provider errors from the fresh bind.
    pub async fn bind_session_runtime(
        &self,
        session: SessionId,
        workdir: &Path,
    ) -> Result<TurnBinding, CoreError> {
        let projection = self.read_projection_shared(session).await?;
        if projection.session.id != Some(session) {
            return Err(CoreError::Invalid(format!("session not found: {session}")));
        }
        let (root, _) = self.session_lineage(session).await?;
        let root_projection = self.read_projection_shared(root).await?;
        if root_projection.session.id != Some(root) {
            return Err(CoreError::Invalid(format!(
                "session root not found: {root}"
            )));
        }
        let binding = self.bind_root_runtime(workdir).await?;
        let overrides = root_projection
            .session
            .agent_model_overrides
            .iter()
            .filter(|(_, model)| self.provider_router().resolve(model).is_some())
            .map(|(agent, model)| (agent.clone(), model.clone()))
            .collect::<BTreeMap<_, _>>();
        Ok(binding.with_session_agent_models(overrides))
    }

    /// Set or clear one temporary model override on the root Session tree.
    ///
    /// The event is always appended to the lineage root so descendants resolve
    /// the same durable map. Existing bindings remain immutable and therefore
    /// continue using the model captured before this mutation.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] when `session` (or its lineage root) is
    /// absent, plus store errors from event emission.
    pub async fn set_agent_model_override(
        &self,
        session: SessionId,
        agent: AgentName,
        model: Option<ModelRef>,
    ) -> Result<(), CoreError> {
        let projection = self.read_projection(session).await?;
        if projection.session.id != Some(session) {
            return Err(CoreError::Invalid(format!("session not found: {session}")));
        }
        let (root, _) = self.session_lineage(session).await?;
        let root_projection = self.read_projection(root).await?;
        if root_projection.session.id != Some(root) {
            return Err(CoreError::Invalid(format!(
                "session root not found: {root}"
            )));
        }
        self.emit(
            root,
            Event::SessionAgentModelOverrideSet {
                session: root,
                agent,
                model,
            },
        )
        .await
    }

    /// Mode used by a session tree whose root records none: `yolo` when the
    /// process invocation model is `danger` (config or `--yolo`), else
    /// `manual`.
    #[must_use]
    pub fn default_permission_mode(&self) -> crate::permission_mode::SessionPermissionMode {
        crate::permission_mode::SessionPermissionMode::process_default(
            self.permission.invocation_model(),
        )
    }

    /// The lineage root of `session` and the permission mode it records
    /// (`None` when the tree uses the process default).
    async fn recorded_permission_mode(
        &self,
        session: SessionId,
    ) -> Result<(SessionId, Option<String>), CoreError> {
        let (root, _) = self.session_lineage(session).await?;
        let mode = self
            .store
            .with_projection(root, |projection| {
                projection.session.permission_mode.clone()
            })
            .await?;
        Ok((root, mode))
    }

    /// Effective permission mode of `session`'s tree in wire form
    /// (`manual`, `yolo`, or `<bundle-id>/<mode-id>`): the root's recorded
    /// mode, else [`Self::default_permission_mode`].
    ///
    /// # Errors
    /// Returns store errors while walking the lineage.
    pub async fn permission_mode(&self, session: SessionId) -> Result<String, CoreError> {
        let (_, recorded) = self.recorded_permission_mode(session).await?;
        Ok(recorded.unwrap_or_else(|| self.default_permission_mode().as_wire()))
    }

    /// Every selectable permission mode: the built-in `manual` and `yolo`,
    /// then each published bundle's declared modes (after refreshing the
    /// runtime catalog so newly installed bundles appear).
    pub async fn permission_modes(&self) -> Vec<crate::permission_mode::PublishedPermissionMode> {
        self.refresh_catalog_for_bundle_api().await;
        let mut modes = crate::permission_mode::builtin_permission_modes();
        modes.extend(self.runtime.published_permission_modes());
        modes
    }

    /// Set the permission mode of `session`'s tree.
    ///
    /// The event is appended to the lineage root, so every descendant
    /// (subagent) session uses the same mode. `mode` must be `manual`,
    /// `yolo`, or a `<bundle-id>/<mode-id>` published by an installed
    /// bundle. The change applies to the next permission check of every
    /// session in the tree, including turns already running. Returns the
    /// root session.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] for an absent session or an unknown or
    /// unavailable mode, plus store errors from event emission.
    pub async fn set_permission_mode(
        &self,
        session: SessionId,
        mode: &str,
    ) -> Result<SessionId, CoreError> {
        let parsed = crate::permission_mode::SessionPermissionMode::parse(mode)
            .ok_or_else(|| CoreError::Invalid(format!("unknown permission mode: {mode:?}")))?;
        if let crate::permission_mode::SessionPermissionMode::Bundle { .. } = &parsed {
            let wire = parsed.as_wire();
            if !self
                .permission_modes()
                .await
                .iter()
                .any(|published| published.id == wire)
            {
                return Err(CoreError::Invalid(format!(
                    "permission mode is not available: {mode:?}"
                )));
            }
        }
        if !self.session_exists(session).await? {
            return Err(CoreError::Invalid(format!("session not found: {session}")));
        }
        let (root, _) = self.session_lineage(session).await?;
        if !self.session_exists(root).await? {
            return Err(CoreError::Invalid(format!(
                "session root not found: {root}"
            )));
        }
        self.emit(
            root,
            Event::SessionPermissionModeSet {
                session: root,
                mode: parsed.as_wire(),
            },
        )
        .await?;
        Ok(root)
    }

    /// Derive one tool call's permission plane for `session` from its tree's
    /// current mode (read now, so a switch applies to the next check).
    ///
    /// A recorded mode this binary cannot parse, or a bundle mode whose
    /// bundle no longer publishes it in `binding`, behaves as `manual`.
    pub(crate) async fn mode_permission_plane(
        &self,
        binding: &TurnBinding,
        session: SessionId,
        agent: Option<&AgentName>,
    ) -> Result<PermissionPlane, CoreError> {
        use crate::permission_mode::{ModeApprover, SessionPermissionMode, derive_plane};
        let (root, recorded) = self.recorded_permission_mode(session).await?;
        let mode = match recorded {
            Some(recorded) => {
                SessionPermissionMode::parse(&recorded).unwrap_or(SessionPermissionMode::Manual)
            }
            None => self.default_permission_mode(),
        };
        let approver = match &mode {
            SessionPermissionMode::Bundle { bundle, mode } => {
                binding.permission_mode_hooks(bundle, mode).map(|hooks| {
                    Arc::new(ModeApprover::new(
                        hooks,
                        root,
                        agent.cloned(),
                        bundle.clone(),
                        mode.clone(),
                    )) as Arc<dyn hya_tool::PermissionInterceptor>
                })
            }
            SessionPermissionMode::Manual | SessionPermissionMode::Yolo => None,
        };
        Ok(derive_plane(&self.permission, &mode, approver))
    }

    /// Resolve a catalog agent into an [`AgentSpec`] using `binding`.
    ///
    /// # Errors
    /// Returns [`CoreError::AgentDefinitionMissing`] or bundle errors.
    pub fn agent_spec_for_binding(
        &self,
        binding: &TurnBinding,
        base: &AgentSpec,
        stable_id: &str,
    ) -> Result<AgentSpec, CoreError> {
        agent_from_definition(base, stable_id, binding)
    }

    /// Build the caller's authorized spawn roster for tools.
    ///
    /// # Errors
    /// Returns catalog/resolution errors.
    pub fn agent_roster_for_binding(
        &self,
        binding: &TurnBinding,
        caller: &str,
    ) -> Result<Arc<[AgentDef]>, CoreError> {
        agent_roster(binding, caller)
    }

    /// Resource/tool policy for `stable_id` under `binding`.
    ///
    /// # Errors
    /// Returns catalog/resolution errors.
    pub fn agent_resource_policy_for_binding(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
    ) -> Result<AgentResourcePolicy, CoreError> {
        Ok(binding.agent_resource_policy(stable_id)?)
    }

    /// Publish a new runtime candidate via the registry builder callback.
    ///
    /// # Errors
    /// Returns [`RuntimeRefreshError`] when the candidate is rejected.
    pub fn refresh_runtime(
        &self,
        build: impl FnOnce(&mut RuntimeCandidate) -> Result<(), RuntimeRefreshError>,
    ) -> Result<hya_proto::ConfigGeneration, RuntimeRefreshError> {
        self.runtime.refresh(build)
    }

    /// Replay the full ordered event log for `session`.
    ///
    /// # Errors
    /// Returns store failures as [`CoreError::Store`].
    pub async fn replay(&self, session: SessionId) -> Result<Vec<Envelope>, CoreError> {
        Ok(self.store.replay(session).await?)
    }

    /// Return whether a Session has a durable event log.
    ///
    /// # Errors
    /// Returns store failures as [`CoreError::Store`].
    pub async fn session_exists(&self, session: SessionId) -> Result<bool, CoreError> {
        Ok(self.store.session_exists(session).await?)
    }

    /// Fold the session log into a projection.
    ///
    /// # Errors
    /// Returns store failures as [`CoreError::Store`].
    pub async fn read_projection(&self, session: SessionId) -> Result<Projection, CoreError> {
        Ok(self.store.read_projection(session).await?)
    }

    /// Shared handle to the session's folded projection, without a deep clone.
    ///
    /// Same fold as [`SessionEngine::read_projection`] (the store's projection
    /// cache); prefer it on hot paths that re-read large team-root logs.
    ///
    /// # Errors
    /// Returns store failures as [`CoreError::Store`].
    pub async fn read_projection_shared(
        &self,
        session: SessionId,
    ) -> Result<Arc<Projection>, CoreError> {
        Ok(self.store.read_projection_shared(session).await?)
    }

    /// Refresh the runtime catalog (when an app refresher is configured) so
    /// bundle API lookups see newly installed bundles. A failed refresh keeps
    /// the live generation and is only logged: a bundle endpoint must not fail
    /// because an unrelated bundle cannot start.
    async fn refresh_catalog_for_bundle_api(&self) {
        if let Some(refresh) = &self.catalog_refresh
            && let Err(error) = refresh.refresh_if_changed(self.runtime.as_ref()).await
        {
            tracing::warn!("runtime catalog refresh before a bundle API call failed: {error:#}");
        }
    }

    /// Every published bundle's declared API endpoints.
    pub async fn bundle_apis(&self) -> Vec<crate::bundle_apis::PublishedBundleApis> {
        self.refresh_catalog_for_bundle_api().await;
        self.runtime.published_bundle_apis()
    }

    /// Serve one bundle API call.
    ///
    /// Checks the session of a session-scoped call, resolves the bundle in
    /// the live published generation, routes the path against its declared
    /// templates, and forwards the request to its process, which reads
    /// through a request-scoped read-only capability (bound to the session for
    /// session scope, to no session for global scope).
    ///
    /// # Errors
    /// [`crate::BundleApiError`] for an unknown session, bundle, or endpoint,
    /// a disallowed method, a malformed call, a process failure, or a store
    /// failure.
    pub async fn invoke_bundle_api(
        &self,
        call: crate::bundle_apis::BundleApiCall,
    ) -> Result<crate::bundle_apis::BundleApiOutcome, crate::BundleApiError> {
        if let Some(session) = call.session
            && !self
                .store
                .session_exists(session)
                .await
                .map_err(CoreError::from)?
        {
            return Err(crate::BundleApiError::SessionNotFound(session));
        }
        self.refresh_catalog_for_bundle_api().await;
        let apis = self.runtime.bundle_apis(&call.bundle).ok_or_else(|| {
            crate::BundleApiError::NotFound {
                bundle: call.bundle.clone(),
                scope: call.scope(),
                path: call.path.clone(),
            }
        })?;
        apis.invoke(call).await
    }

    /// Walk the `SessionCreated{parent}` chain to the top ancestor, returning the
    /// root session and this session's depth (0 = no parent / interactive lead,
    /// 1 = a direct subagent, and so on). Depth is derived from the replayed
    /// projection so there is no separate stored value that can drift. Bounded by a
    /// generous iteration cap as a cycle/runaway guard.
    pub async fn session_lineage(&self, session: SessionId) -> Result<(SessionId, u32), CoreError> {
        let mut current = session;
        let mut depth = 0u32;
        for _ in 0..1024 {
            let parent = self
                .store
                .with_projection(current, |projection| projection.session.parent)
                .await?;
            match parent {
                Some(parent) => {
                    current = parent;
                    depth = depth.saturating_add(1);
                }
                None => break,
            }
        }
        Ok((current, depth))
    }

    async fn emit(&self, session: SessionId, event: Event) -> Result<(), CoreError> {
        let record_usage = matches!(
            &event,
            Event::MessageFinished {
                role: Role::Assistant,
                ..
            }
        );
        let (seq, ts_millis) = self.store.append_event(session, &event).await?;
        self.publish_envelope(Envelope {
            seq,
            ts_millis,
            event,
        });
        if record_usage {
            // The ledger is best-effort: a recording failure must never fail
            // the turn that already finished.
            if let Err(error) = self.record_session_usage(session).await {
                tracing::warn!("usage ledger recording failed: {error:#}");
            }
        }
        Ok(())
    }

    /// Append one `UsageRecorded` side-call record per call drained from `usage`.
    ///
    /// Side calls (title, summarizer) carry no message or step and never touch
    /// the transcript. Best-effort: a failed append is logged, never surfaced,
    /// so accounting cannot fail the work it describes.
    pub(crate) async fn record_side_call_usage(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        purpose: hya_proto::UsagePurpose,
        usage: &crate::compaction::UsageCollector,
    ) {
        for (model, tokens) in usage.take() {
            let event = Event::UsageRecorded {
                session,
                message: None,
                step: None,
                model,
                purpose,
                tokens,
            };
            if let Err(error) = self.emit_for_actor(actor_claim, session, event).await {
                tracing::warn!(%session, "side-call usage record was not appended: {error:#}");
            }
        }
    }

    /// Record one token-ledger row for the just-finished assistant message.
    ///
    /// Provider-reported usage wins (`confidence: provider`): the sum of the
    /// message's attributed rounds (`UsageRecorded`, present even on a
    /// cancelled or errored message) with the model that served the latest
    /// round, else the legacy `MessageFinished.tokens` with the session model.
    /// `prompt_tokens` is the whole prompt, `input + cache_read + cache_write`.
    /// Otherwise the turn's texts are counted with the model family's real
    /// tokenizer when one resolves (`confidence: hf:<repo>`) or the calibrated
    /// estimator (`confidence: estimated`). Always records — never skips.
    ///
    /// The ledger is a best-effort side table; per-model truth for a message
    /// whose rounds ran on several models lives in `SessionProjection.usage`.
    async fn record_session_usage(&self, session: SessionId) -> Result<(), CoreError> {
        use hya_proto::{MessageProjection, PartProjection};

        let projection = self.store.read_projection_shared(session).await?;
        let Some(message) = projection
            .session
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::Assistant)
        else {
            return Ok(());
        };
        let part_text = |message: &MessageProjection| {
            message
                .parts
                .iter()
                .map(|part| match part {
                    PartProjection::Text { text, .. } => text.as_str(),
                    _ => "",
                })
                .collect::<String>()
        };
        let completion_text = part_text(message);
        let prompt_text = projection
            .session
            .messages
            .iter()
            .take_while(|candidate| candidate.id != message.id)
            .map(part_text)
            .collect::<Vec<_>>()
            .join("\n");

        let reported = match (&message.usage, message.tokens) {
            (Some(attributed), _) => Some((attributed.tokens, Some(&attributed.model))),
            (None, Some(tokens)) => Some((tokens, None)),
            (None, None) => None,
        };
        let model = reported
            .and_then(|(_, served)| served)
            .or(projection.session.model.as_ref())
            .map(|model| model.as_str().to_string())
            .unwrap_or_default();
        let provider = model
            .split('/')
            .next()
            .map(str::to_string)
            .filter(|part| !model.is_empty() && part != model.as_str());
        let role = projection
            .session
            .agent
            .as_ref()
            .map(|agent| agent.as_str().to_string())
            .unwrap_or_else(|| "assistant".to_string());

        let entry = match reported {
            Some((usage, _)) => hya_store::LedgerEntry {
                session,
                role,
                iteration: None,
                completion_run_id: None,
                prompt_tokens: usage.prompt() as i64,
                completion_tokens: usage.output as i64,
                confidence: "provider".to_string(),
                provider,
                model: (!model.is_empty()).then_some(model),
            },
            None => {
                let tokenizer = self.usage_tokenizers.tokenizer_for_model(&model);
                let name = tokenizer.name().to_string();
                hya_store::LedgerEntry {
                    session,
                    role,
                    iteration: None,
                    completion_run_id: None,
                    prompt_tokens: tokenizer.count_text(&prompt_text) as i64,
                    completion_tokens: tokenizer.count_text(&completion_text) as i64,
                    confidence: if name == "calibrated" {
                        "estimated".to_string()
                    } else {
                        format!("hf:{name}")
                    },
                    provider,
                    model: (!model.is_empty()).then_some(model),
                }
            }
        };
        self.store.record_usage(&entry).await?;
        Ok(())
    }

    pub(crate) async fn emit_for_actor(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        event: Event,
    ) -> Result<(), CoreError> {
        match actor_claim {
            Some(claim) => {
                self.commit_resident_mutation(claim, session, vec![event])
                    .await
            }
            None => self.emit(session, event).await,
        }
    }

    pub(crate) async fn validate_actor_claim(
        &self,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<(), CoreError> {
        if let Some(claim) = actor_claim {
            self.store.validate_actor_claim(claim).await?;
        }
        Ok(())
    }

    /// Commit resident-owned canonical events only while the supplied actor
    /// capability is current. Publication happens after the SQLite transaction,
    /// so a stale completion cannot advance live observers or replay state.
    pub async fn commit_resident_mutation(
        &self,
        claim: &ActorClaim,
        session: SessionId,
        events: Vec<Event>,
    ) -> Result<(), CoreError> {
        let finished_assistant = events.iter().any(|event| {
            matches!(
                event,
                Event::MessageFinished {
                    role: Role::Assistant,
                    ..
                }
            )
        });
        let envelopes = self
            .store
            .commit_resident_mutation(claim, session, &events)
            .await?;
        for envelope in envelopes {
            self.publish_envelope(envelope);
        }
        if finished_assistant && let Err(error) = self.record_session_usage(session).await {
            tracing::warn!("usage ledger recording failed: {error:#}");
        }
        Ok(())
    }

    fn publish_live(&self, event: Event) {
        self.publish_envelope(Envelope {
            seq: EventSeq(0),
            ts_millis: now_millis(),
            event,
        });
    }

    pub(crate) fn publish_envelope(&self, envelope: Envelope) {
        if let Some(hooks) = &self.hooks {
            hooks.dispatch_event(&envelope);
        }
        let session = envelope.event.session();
        let active = session.and_then(activation_hook_for);
        let captured = session.and_then(|session| {
            self.session_bundle_hooks
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&session)
                .cloned()
        });
        if let Some(hooks) = active.or(captured) {
            hooks.dispatch_event(&envelope);
        }
        self.bus.publish(envelope);
    }

    /// Create a new session id and append `SessionCreated`.
    ///
    /// # Errors
    /// Returns store/append failures.
    pub async fn create(&self, spec: CreateSession) -> Result<SessionId, CoreError> {
        self.create_with_id(None, spec).await
    }

    /// Create a session under a resident actor claim (fenced append).
    ///
    /// # Errors
    /// Returns claim validation or store failures.
    #[doc(hidden)]
    pub async fn create_for_actor(
        &self,
        claim: &ActorClaim,
        spec: CreateSession,
    ) -> Result<SessionId, CoreError> {
        let id = SessionId::new();
        self.commit_resident_mutation(
            claim,
            id,
            vec![Event::SessionCreated {
                session: id,
                parent: spec.parent,
                agent: spec.agent,
                model: spec.model,
                workdir: spec.workdir,
            }],
        )
        .await?;
        if let Some(hooks) = &self.hooks {
            hooks
                .session_start(SessionLifecycleInput { session: id })
                .await;
        }
        Ok(id)
    }

    /// Create or re-open a session with an optional fixed id (idempotent if log non-empty).
    ///
    /// # Errors
    /// Returns store/append failures.
    pub async fn create_with_id(
        &self,
        id: Option<SessionId>,
        spec: CreateSession,
    ) -> Result<SessionId, CoreError> {
        let id = id.unwrap_or_default();
        if !self.replay(id).await?.is_empty() {
            return Ok(id);
        }
        let is_root = spec.parent.is_none();
        let stable_agent_id = spec.agent.as_str().to_string();
        let workdir = PathBuf::from(&spec.workdir);
        self.emit(
            id,
            Event::SessionCreated {
                session: id,
                parent: spec.parent,
                agent: spec.agent,
                model: spec.model,
                workdir: spec.workdir,
            },
        )
        .await?;
        if is_root {
            match self.bind_session_runtime(id, &workdir).await {
                Ok(binding) => {
                    self.capture_session_bundle_hooks(id, &binding, &stable_agent_id)
                        .await;
                }
                Err(error) => {
                    tracing::warn!(session = %id, %error, "session.start bundle hook binding failed");
                }
            }
        }
        if let Some(hooks) = &self.hooks {
            hooks
                .session_start(SessionLifecycleInput { session: id })
                .await;
        }
        Ok(id)
    }

    /// Notify session lifecycle hooks. Best-effort by contract: implementors
    /// log their own failures, and the engine never propagates them.
    async fn notify_session_lifecycle(&self, session: SessionId, start: bool) {
        if let Some(hooks) = &self.hooks {
            let input = SessionLifecycleInput { session };
            if start {
                hooks.session_start(input).await;
            } else {
                hooks.session_end(input).await;
            }
        }
        let bundle_hooks = if start {
            self.session_bundle_hooks
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&session)
                .cloned()
        } else {
            self.session_channel_policies
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&session);
            self.session_bundle_hooks
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&session)
        };
        if let Some(hooks) = bundle_hooks {
            let input = SessionLifecycleInput { session };
            if start {
                hooks.session_start(input).await;
            } else {
                hooks.session_end(input).await;
            }
        }
    }

    pub(crate) async fn capture_session_bundle_hooks(
        &self,
        session: SessionId,
        binding: &TurnBinding,
        stable_agent_id: &str,
    ) {
        let channel_policy = crate::ChannelPolicy::from_binding(binding)
            .map(|policy| policy.snapshot_for(stable_agent_id))
            .unwrap_or_default();
        self.session_channel_policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session, channel_policy);
        let hooks = binding.bundle_hooks_for_agent(stable_agent_id);
        if hooks.is_empty() {
            return;
        }
        let dispatcher = Arc::new(HookChain::new(hooks)) as Arc<dyn HookDispatcher>;
        let inserted = {
            let mut captured = self
                .session_bundle_hooks
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let std::collections::hash_map::Entry::Vacant(entry) = captured.entry(session) {
                entry.insert(Arc::clone(&dispatcher));
                true
            } else {
                false
            }
        };
        if inserted {
            dispatcher
                .session_start(SessionLifecycleInput { session })
                .await;
        }
    }

    pub(crate) fn session_channel_policy(
        &self,
        session: SessionId,
    ) -> Option<hya_tool::ChannelPolicySnapshot> {
        self.session_channel_policies
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session)
            .copied()
    }

    pub(crate) fn update_session_channel_policy(
        &self,
        session: SessionId,
        policy: hya_tool::ChannelPolicySnapshot,
    ) {
        self.session_channel_policies
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session, policy);
    }

    pub(crate) fn session_hook_dispatcher(
        &self,
        session: SessionId,
    ) -> Option<Arc<dyn HookDispatcher>> {
        let mut dispatchers = Vec::new();
        if let Some(hooks) = &self.hooks {
            dispatchers.push(Arc::clone(hooks));
        }
        if let Some(hooks) = self
            .session_bundle_hooks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session)
            .cloned()
        {
            dispatchers.push(hooks);
        }
        match dispatchers.len() {
            0 => None,
            1 => dispatchers.pop(),
            _ => Some(Arc::new(HookChain::new(dispatchers))),
        }
    }

    /// Startup hooks followed by the immutable activation-bound hook chain.
    pub(crate) fn active_hook_dispatcher(
        &self,
        session: SessionId,
    ) -> Option<Arc<dyn HookDispatcher>> {
        let mut dispatchers = Vec::new();
        if let Some(hooks) = &self.hooks {
            dispatchers.push(Arc::clone(hooks));
        }
        if let Some(hooks) = activation_hook_for(session) {
            dispatchers.push(hooks);
        }
        match dispatchers.len() {
            0 => None,
            1 => dispatchers.pop(),
            _ => Some(Arc::new(HookChain::new(dispatchers))),
        }
    }

    /// Delete a session log from the store.
    ///
    /// # Errors
    /// Returns store failures.
    pub async fn delete_session(&self, session: SessionId) -> Result<bool, CoreError> {
        // Root-turn teardown (ADR-0015 §1): deleting a team root force-archives
        // every live descendant (claims release, roster exits) before the log
        // goes away. Best-effort — the store delete proceeds regardless.
        if let Ok((root, 0)) = self.session_lineage(session).await {
            let _ = self.force_archive_team(root).await;
        }
        let deleted = self.store.delete_session(session).await?;
        if deleted {
            self.notify_session_lifecycle(session, false).await;
        }
        Ok(deleted)
    }
}

pub(crate) fn effective_agent_for_binding(
    agent: &AgentSpec,
    stable_id: &str,
    binding: &TurnBinding,
    guidance: Option<&str>,
) -> Result<(AgentSpec, Arc<CompiledResourceView>), CoreError> {
    effective_agent_for_binding_with_sidecar_tools(agent, stable_id, binding, guidance, &[])
}

pub(crate) fn effective_agent_for_binding_with_sidecar_tools(
    agent: &AgentSpec,
    stable_id: &str,
    binding: &TurnBinding,
    guidance: Option<&str>,
    sidecar_tools: &[ResolvedTool],
) -> Result<(AgentSpec, Arc<CompiledResourceView>), CoreError> {
    // One composition seam: agent_base (Bundle Some replaces / None keeps
    // Harness base) → nonempty guidance → skill prompt material.
    let effective = agent_from_definition(agent, stable_id, binding)?;
    let effective = agent_with_guidance_layer(effective, guidance);
    let policy = binding.agent_resource_policy(stable_id)?;
    let resources = if sidecar_tools.is_empty() {
        binding.compile_agent_resources(&policy)?
    } else {
        binding.compile_agent_resources_with_sidecar_tools(&policy, sidecar_tools)?
    };
    Ok((
        agent_with_bound_skills(effective, resources.as_ref()),
        resources,
    ))
}

fn agent_from_definition(
    agent: &AgentSpec,
    stable_id: &str,
    binding: &TurnBinding,
) -> Result<AgentSpec, CoreError> {
    let definition =
        binding
            .resolve_agent(stable_id)
            .ok_or_else(|| CoreError::AgentDefinitionMissing {
                agent_id: stable_id.to_string(),
            })?;
    let mut effective = agent.clone();
    effective.name = AgentName::new(definition.stable_id);
    effective.workdir = binding.workdir().to_path_buf();
    // Agent prompt Some replaces only agent_base; None preserves Harness base.
    if let Some(prompt) = definition.prompt {
        effective.system_prompt = prompt.to_string();
    }
    if let Some(reasoning) = definition
        .model_policy
        .reasoning
        .as_deref()
        .and_then(hya_provider::ReasoningEffort::parse)
    {
        effective.reasoning = Some(reasoning);
    }
    Ok(effective)
}

/// Append nonempty request-scoped guidance after agent_base resolution.
///
/// Absence or empty text is an empty layer (no error). Callers pre-render once
/// per turn; this does not discover files.
pub(crate) fn agent_with_guidance_layer(mut agent: AgentSpec, guidance: Option<&str>) -> AgentSpec {
    let Some(guidance) = guidance.map(str::trim).filter(|text| !text.is_empty()) else {
        return agent;
    };
    let base = agent.system_prompt.trim_end();
    agent.system_prompt = if base.is_empty() {
        guidance.to_string()
    } else {
        format!("{base}\n\n{guidance}")
    };
    agent
}

/// Exact-lookup a fixed Harness system agent from a captured TurnBinding.
///
/// Accepts only [`FixedSystemAgent`] — callers cannot pass an arbitrary ID.
/// Not agent spawn and not a generic bypass surface.
fn fixed_system_agent(
    binding: &TurnBinding,
    agent: FixedSystemAgent,
) -> Result<AgentDefinition<'_>, CoreError> {
    let stable_id = agent.stable_id();
    binding
        .resolve_agent(stable_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_id.to_string(),
        })
}

/// Build summarize options from a fixed system definition and bound preference.
///
/// Prepared prompt, configured direct/category policy, and explicit reasoning
/// apply when present. An eligible remembered model replaces the caller or
/// summarizer fallback only when the definition has no configured route.
pub(crate) fn summarize_options_from_definition(
    definition: &AgentDefinition<'_>,
    categories: &CategoryRegistry,
    preference: Option<&ModelRef>,
    is_servable: &dyn Fn(&ModelRef) -> bool,
) -> SummarizeOptions {
    SummarizeOptions {
        system: definition.prompt.map(str::to_string),
        model: crate::category::resolve_configured_agent_model(
            &definition.model_policy,
            categories,
            is_servable,
        )
        .or_else(|| {
            crate::category::eligible_agent_model_preference(definition, preference, is_servable)
                .cloned()
        }),
        reasoning: definition
            .model_policy
            .reasoning
            .as_deref()
            .and_then(ReasoningEffort::parse),
        // Anchoring, output budget, and handoff mode are per-call, not
        // per-definition: the caller knows the transcript and the active
        // compaction config.
        previous_summary: None,
        max_output_tokens: None,
        handoff: false,
        state_only: false,
        // Callers that bill a session attach their own collector.
        usage: None,
    }
}

pub(crate) fn projection_workdir(projection: &Projection) -> Option<PathBuf> {
    projection.session.workdir.as_ref().map(PathBuf::from)
}

pub(crate) fn agent_with_bound_skills(
    mut effective: AgentSpec,
    resources: &CompiledResourceView,
) -> AgentSpec {
    if let Some(section) = resources.skills_prompt_section() {
        let prompt = effective.system_prompt.trim_end();
        effective.system_prompt = if prompt.is_empty() {
            section
        } else {
            format!("{prompt}\n\n{section}")
        };
    }
    effective
}

fn agent_roster(binding: &TurnBinding, caller: &str) -> Result<Arc<[AgentDef]>, CoreError> {
    Ok(binding
        .spawnable_agents(caller)?
        .iter()
        .map(agent_def)
        .collect::<Vec<_>>()
        .into())
}

fn agent_def(agent: &AgentDefinition<'_>) -> AgentDef {
    AgentDef {
        name: agent.stable_id.to_string(),
        description: agent.description.map(str::to_string),
        category: agent.model_policy.category.clone(),
        mode: agent.selector_mode().to_string(),
    }
}

pub(crate) fn session_workdir(agent: &AgentSpec, projection: &Projection) -> PathBuf {
    projection
        .session
        .workdir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| agent.workdir.clone())
}
