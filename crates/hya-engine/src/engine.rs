use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use hya_proto::{MessageId, ModelRef, SessionId, ToolCallId};
use hya_provider::{ProviderRouter, ReasoningEffort};
use hya_store::SessionStore;
use hya_tool::handle::ArtifactPlane;
use hya_tool::{
    FormatterPlane, InteractionPlane, LifecyclePlane, LspPlane, MailboxPlane, PermissionPlane,
    ResolvedTool, ToolError,
};
use serde_json::Value;

#[cfg(test)]
use tokio::sync::Notify;

use crate::agent_catalog::AgentDefinition;
use crate::bus::EventBus;
use crate::compaction::CompactionConfig;
use crate::error::CoreError;
use crate::hooks::HookDispatcher;
use crate::sidecar::SidecarEnvironment;
use crate::tokens::TokenAccounting;
use crate::{CategoryRegistry, RuntimeRegistry, TurnBinding};

/// Closed set of fixed Harness system-operation agents.
///
/// Exact catalog lookup only — not spawn, not roster, and not an arbitrary-ID
/// bypass. Callers cannot pass an open string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixedSystemAgent {
    Title,
}

impl FixedSystemAgent {
    /// Stable catalog id for this fixed system operation.
    const fn stable_id(self) -> &'static str {
        match self {
            Self::Title => "hya-title",
        }
    }
}

mod admission;
mod agent_binding;
mod channel_host;
mod events;
mod file_snapshot;
mod fork;
mod mailbox;
mod member_control;
mod members;
mod model_probe;
mod models;
#[cfg(unix)]
#[cfg(test)]
mod native_rewriters_test;
mod nested_tools;
mod permissions;
mod restart;
mod revert;
mod roots;
mod scope_binding;
mod session_hooks;
mod session_state;
mod session_title;
mod shell;
mod spawn_requests;
mod steer;
mod stream_round;
mod summary;
/// Model-backed terminal handoff documents for resident agents.
pub mod terminal_handoff;
mod text_complete;
mod tool_error;
mod turn;
mod turn_end;
mod turn_gate;

pub use agent_binding::AgentSpec;
use agent_binding::{agent_roster, agent_with_bound_skills};
pub(crate) use agent_binding::{
    agent_with_guidance_layer, effective_agent_for_binding,
    effective_agent_for_binding_with_sidecar_tools,
};
pub(crate) use mailbox::dm_channel_between;
pub use member_control::ArchiveReviver;
pub use model_probe::{MODEL_PROBE_PROMPT, ModelProbeReply};
pub use models::ProviderSnapshot;
use models::{ProviderSnapshots, TurnEffortFallback};
pub use scope_binding::RuntimeCatalogRefresh;
pub use session_state::CreateSession;
pub(crate) use session_state::projection_workdir;
use session_state::session_workdir;
pub use spawn_requests::{BoundSpawnRequest, BoundSpawnSender};
use terminal_handoff::{SummarizeOptions, Summarizer, TerminalHandoffOptions};
pub(crate) use turn::TurnRequestContext;
pub use turn::{ExplicitModelRoute, ExplicitModelRouteCandidate, ExplicitTurnRoute};

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

pub use file_snapshot::{MAX_DIRTY_BYTES, MAX_DIRTY_FILES, MAX_FILE_BYTES, MAX_SESSION_BLOB_BYTES};
pub use fork::{ForkAt, ForkError, fork_cut};
pub use revert::{RevertError, RevertOutcome, RevertTarget};
pub use scope_binding::{BundleRefresh, CatalogScopeCacheConfig};
pub use turn::advertise_tool;
pub use turn_end::{DRAIN_DEADLINE, TurnDrainReport};
pub use turn_gate::{TurnBoundaryObserver, TurnLease};

#[cfg(test)]
pub(crate) struct MailPreAppendGate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[cfg(test)]
impl MailPreAppendGate {
    pub(crate) fn new(entered: Arc<Notify>, release: Arc<Notify>) -> Self {
        Self { entered, release }
    }
}

/// Central session runtime: persistence, providers, tools, and turn execution.
///
/// Construct with [`SessionEngine::new`], then chain `with_*` builders for
/// interaction, spawn, mailbox, hooks, compaction, and governors. Turns append
/// events to the store and publish on the bus; observers never write the log.
///
/// Clones share execution handles and live provider publications.
pub struct SessionEngine {
    workspace_watches: hya_core::kernel::WorkspaceWatches,
    store: SessionStore,
    base_system_prompt: String,
    pure_context: bool,
    /// Whether system prompts may consult first-party tool-family exposure
    /// policies (`hya_tool_loader`). Disabled in bare mode so zero builtin
    /// bundles are read from disk.
    use_tool_family_prompts: bool,
    providers: ProviderSnapshots,
    /// Cross-model failover plane: preferred [`ModelRef`] → its ordered
    /// candidate chain (the preferred model itself first). Empty by default,
    /// which keeps turn streaming byte-identical to a direct router call.
    model_fallbacks: HashMap<ModelRef, Vec<ModelRef>>,
    /// Process-wide reasoning fallback applied after model defaults.
    global_reasoning: Option<ReasoningEffort>,
    /// A tool that, for an agent whose view holds it, is the only one offered
    /// to the model; the agent's other tools stay executable.
    exclusive_tool: Option<Arc<str>>,
    /// Configured Agent model categories used by fixed system-Agent calls.
    model_categories: Arc<CategoryRegistry>,
    runtime: Arc<RuntimeRegistry>,
    catalog_refresh: Option<Arc<dyn RuntimeCatalogRefresh>>,
    /// Scope overlay last-bind times and invalidation fan-out, shared by
    /// every clone of this engine.
    scope_cache: Arc<scope_binding::ScopeCache>,
    permission: PermissionPlane,
    interaction: InteractionPlane,
    spawner: BoundSpawnSender,
    native_services: Arc<RwLock<Vec<Arc<dyn hya_runtime_abi::NativeService>>>>,
    mailbox: MailboxPlane,
    project_activity: hya_tool::ProjectActivityPlane,
    lifecycle: LifecyclePlane,
    /// Mints subagent handle leaves (`<prefix>-<operator>`); injectable RNG.
    handle_namer: Arc<crate::handle_naming::HandleNamer>,
    /// User-registered `artifact://` post-processing, carried to every tool call.
    artifacts: ArtifactPlane,
    formatter: FormatterPlane,
    lsp: LspPlane,
    bus: EventBus,
    summarizer: Option<Arc<dyn Summarizer>>,
    handoff_options: TerminalHandoffOptions,
    rewriter_keep_recent: usize,
    token_accounting: TokenAccounting,
    /// Context-reduction extensions the kernel runs before its own Rewrite,
    /// before each round and on `/summarize`.
    rewriters: Arc<[Arc<dyn hya_context::rewrite::Rewriter>]>,
    /// Process-wide ABI v2 native bundle hooks (run before external plugin hooks).
    native_hooks: Option<Arc<dyn HookDispatcher>>,
    hooks: Option<Arc<dyn HookDispatcher>>,
    /// Captured per-session lifecycle/event hooks ([`session_hooks`]).
    session_bundle_hooks: Arc<RwLock<session_hooks::SessionHookMap>>,
    session_channel_policies: Arc<RwLock<HashMap<SessionId, hya_tool::ChannelPolicySnapshot>>>,
    mail_steer_suppressed: Arc<RwLock<HashMap<SessionId, bool>>>,
    staged_mail_acks: Arc<RwLock<HashMap<SessionId, hya_runtime_abi::StagedMailAck>>>,
    governor: Option<crate::orchestrator::SubagentGovernor>,
    sidecar_environment: Option<Arc<dyn SidecarEnvironment>>,
    /// Revival seam (ADR-0015): a downward mail to an archived direct child
    /// routes here instead of failing the send. Interior-mutable because the
    /// implementor (resident supervisor) itself holds the engine —
    /// [`ResidentSupervisor::start`](crate::resident::ResidentSupervisor::start)
    /// wires it.
    reviver: Arc<RwLock<Option<std::sync::Weak<crate::resident::ResidentSupervisor>>>>,
    /// Runtime-owner claim used for durable stop/start records.
    runtime_owner: hya_proto::OwnerRunId,
    /// Single-active-turn registry shared by every clone of this engine.
    turn_gate: Arc<turn_gate::TurnGate>,
    /// Per-retry backoff schedule for transient/interrupted provider rounds.
    round_retry_waits: &'static [Duration],
    #[cfg(test)]
    mail_pre_append_gate: Option<Arc<MailPreAppendGate>>,
}

impl Clone for SessionEngine {
    fn clone(&self) -> Self {
        Self {
            workspace_watches: self.workspace_watches.clone(),
            store: self.store.clone(),
            base_system_prompt: self.base_system_prompt.clone(),
            pure_context: self.pure_context,
            use_tool_family_prompts: self.use_tool_family_prompts,
            providers: self.providers.clone(),
            global_reasoning: self.global_reasoning,
            exclusive_tool: self.exclusive_tool.clone(),
            model_fallbacks: self.model_fallbacks.clone(),
            model_categories: self.model_categories.clone(),
            runtime: self.runtime.clone(),
            catalog_refresh: self.catalog_refresh.clone(),
            scope_cache: Arc::clone(&self.scope_cache),
            permission: self.permission.clone(),
            interaction: self.interaction.clone(),
            spawner: self.spawner.clone(),
            native_services: Arc::clone(&self.native_services),
            mailbox: self.mailbox.clone(),
            project_activity: self.project_activity.clone(),
            lifecycle: self.lifecycle.clone(),
            handle_namer: self.handle_namer.clone(),
            artifacts: self.artifacts.clone(),
            formatter: self.formatter.clone(),
            lsp: self.lsp.clone(),
            bus: self.bus.clone(),
            summarizer: self.summarizer.clone(),
            handoff_options: self.handoff_options,
            rewriter_keep_recent: self.rewriter_keep_recent,
            token_accounting: self.token_accounting.clone(),
            rewriters: Arc::clone(&self.rewriters),
            native_hooks: self.native_hooks.clone(),
            hooks: self.hooks.clone(),
            session_bundle_hooks: Arc::clone(&self.session_bundle_hooks),
            session_channel_policies: Arc::clone(&self.session_channel_policies),
            mail_steer_suppressed: Arc::clone(&self.mail_steer_suppressed),
            staged_mail_acks: Arc::clone(&self.staged_mail_acks),
            governor: self.governor.clone(),
            sidecar_environment: self.sidecar_environment.clone(),
            reviver: Arc::clone(&self.reviver),
            runtime_owner: self.runtime_owner,
            turn_gate: Arc::clone(&self.turn_gate),
            round_retry_waits: self.round_retry_waits,
            #[cfg(test)]
            mail_pre_append_gate: self.mail_pre_append_gate.clone(),
        }
    }
}

impl SessionEngine {
    /// Set transient MCP sources for this session alone. Pass an empty list
    /// when the owning transport closes.
    pub fn set_session_mcp_sources(&self, session: SessionId, sources: Vec<crate::RuntimeSource>) {
        self.runtime.set_session_sources(session, sources);
    }

    /// Build an engine with disconnected mailbox/spawner and default tool planes.
    ///
    /// Wire product planes with `with_*` before serving interactive traffic.
    ///
    /// # Panics
    /// Panics if an internal rewriter cannot load. Use [`Self::try_new`] at startup
    /// boundaries that return errors.
    #[must_use]
    pub fn new(
        store: SessionStore,
        providers: Arc<ProviderRouter>,
        runtime: Arc<RuntimeRegistry>,
        permission: PermissionPlane,
        bus: EventBus,
    ) -> Self {
        Self::try_new(store, providers, runtime, permission, bus)
            .unwrap_or_else(|error| panic!("load internal bundles: {error}"))
    }

    /// Build the engine after loading the required native rewriters and
    /// projection reducers from internal ABI v2 bundles.
    ///
    /// # Errors
    /// Returns [`CoreError::BundleRuntime`] when an internal bundle cannot load.
    pub fn try_new(
        store: SessionStore,
        providers: Arc<ProviderRouter>,
        runtime: Arc<RuntimeRegistry>,
        permission: PermissionPlane,
        bus: EventBus,
    ) -> Result<Self, CoreError> {
        let (rewriters, reducers) = internal_bundles()?;
        let native_hooks = default_native_hook_list();
        Ok(Self::build_with_rewriters(
            store,
            providers,
            runtime,
            permission,
            bus,
            rewriters,
            native_hooks,
            reducers,
            true,
        ))
    }

    /// Build the engine loading internal ABI v2 bundles with a caller-supplied
    /// [`hya_runtime_abi::HostRuntime`] (for example to expose per-bundle
    /// `config.yml` files to `hya/compaction`).
    ///
    /// # Errors
    /// Returns [`CoreError::BundleRuntime`] when an internal bundle cannot load.
    pub fn try_new_with_host_runtime(
        store: SessionStore,
        providers: Arc<ProviderRouter>,
        runtime: Arc<RuntimeRegistry>,
        permission: PermissionPlane,
        bus: EventBus,
        host_runtime: Arc<dyn hya_runtime_abi::HostRuntime>,
    ) -> Result<Self, CoreError> {
        let regs = hya_core::bundles::load_internal_with_runtime(host_runtime)?;
        let rewriters: NativeRewriters = regs.rewriters.into();
        let native_hooks = default_native_hook_list();
        Ok(Self::build_with_rewriters(
            store,
            providers,
            runtime,
            permission,
            bus,
            rewriters,
            native_hooks,
            regs.projection_reducers,
            true,
        ))
    }

    /// Build the engine in bare mode without loading any internal native
    /// bundles (such as `hya/compaction`) or tool-family prompt presets
    /// (ADR-0039). Context compaction relies solely on the kernel's `rewrite`
    /// primitive.
    #[must_use]
    pub fn new_bare(
        store: SessionStore,
        providers: Arc<ProviderRouter>,
        runtime: Arc<RuntimeRegistry>,
        permission: PermissionPlane,
        bus: EventBus,
    ) -> Self {
        Self::build_with_rewriters(
            store,
            providers,
            runtime,
            permission,
            bus,
            Arc::from([]),
            Vec::new(),
            Vec::new(),
            false,
        )
    }

    /// Control whether system prompts consult first-party tool-family exposure
    /// policies (`hya_tool_loader`).
    #[must_use]
    pub fn with_tool_family_prompts(mut self, enabled: bool) -> Self {
        self.use_tool_family_prompts = enabled;
        self
    }

    #[allow(clippy::too_many_arguments)]
    fn build_with_rewriters(
        store: SessionStore,
        providers: Arc<ProviderRouter>,
        runtime: Arc<RuntimeRegistry>,
        permission: PermissionPlane,
        bus: EventBus,
        rewriters: NativeRewriters,
        mut native_hooks: Vec<Arc<dyn hya_runtime_abi::NativeHook>>,
        abi_reducers: Vec<Arc<dyn hya_runtime_abi::ProjectionReducer>>,
        use_tool_family_prompts: bool,
    ) -> Self {
        if store.projection_reducers().is_empty() {
            store.set_projection_reducers(hya_tool::extension::projection_reducers());
        }
        for reducer in abi_reducers {
            store.register_projection_reducer(Arc::new(AbiV2ProjectionReducerAdapter {
                inner: reducer,
            }));
        }
        let (interaction, _rx) = InteractionPlane::new();
        let spawner = BoundSpawnSender::disconnected();
        let lifecycle = LifecyclePlane::disconnected();
        let mailbox = MailboxPlane::disconnected();
        let (project_activity, activity_rx) = hya_tool::ProjectActivityPlane::new();
        let formatter = FormatterPlane::default();
        let lsp = LspPlane::default();
        let session_channel_policies = Arc::new(RwLock::new(HashMap::new()));
        let mail_steer_suppressed = Arc::new(RwLock::new(HashMap::new()));
        let staged_mail_acks = Arc::new(RwLock::new(HashMap::new()));
        let reviver = Arc::new(RwLock::new(None));
        let runtime_owner = hya_proto::OwnerRunId::new();
        let turn_gate = Arc::new(turn_gate::TurnGate::default());
        let mut artifacts = ArtifactPlane::default();

        if use_tool_family_prompts {
            let channels_runtime: Arc<dyn hya_runtime_abi::HostRuntime> =
                Arc::new(channel_host::EngineChannelsHostRuntime {
                    store: store.clone(),
                    bus: bus.clone(),
                    runtime_owner,
                    session_channel_policies: Arc::clone(&session_channel_policies),
                    mail_steer_suppressed: Arc::clone(&mail_steer_suppressed),
                    staged_mail_acks: Arc::clone(&staged_mail_acks),
                    reviver: Arc::clone(&reviver),
                    turn_gate: Arc::clone(&turn_gate),
                });
            let channels_service =
                Arc::new(hya_agent_channels::AgentChannelsService::with_runtime(
                    Arc::clone(&channels_runtime),
                ));
            native_hooks.push(Arc::new(hya_agent_channels::UnreadMailSteerHook::new(
                Arc::clone(&channels_service),
            )));
            let channel_uri: Arc<dyn hya_runtime_abi::UriSchemaHandler> = Arc::new(
                hya_agent_channels::ChannelUriSchemaHandler::new(channels_service),
            );
            if let Ok(updated) =
                artifacts
                    .clone()
                    .with_uri_handler(hya_runtime_abi::adapt_uri_schema_handler(
                        channel_uri,
                        Some(channels_runtime),
                    ))
            {
                artifacts = updated;
            }
            store.register_projection_reducer(Arc::new(AbiV2ProjectionReducerAdapter {
                inner: Arc::new(hya_agent_channels::AgentChannelsProjectionReducer),
            }));
        }

        let native_hooks = (!native_hooks.is_empty()).then(|| {
            Arc::new(crate::hooks::NativeHookDispatcher::new(native_hooks))
                as Arc<dyn HookDispatcher>
        });

        let engine = Self {
            workspace_watches: Default::default(),
            store,
            providers: ProviderSnapshots::new(providers),
            base_system_prompt: String::new(),
            pure_context: false,
            use_tool_family_prompts,

            global_reasoning: None,
            exclusive_tool: None,
            model_fallbacks: HashMap::new(),
            model_categories: Arc::new(CategoryRegistry::default()),
            runtime,
            catalog_refresh: None,
            scope_cache: Arc::new(scope_binding::ScopeCache::new()),
            permission,
            interaction,
            spawner,
            native_services: Arc::new(RwLock::new(Vec::new())),
            mailbox,
            project_activity,
            lifecycle,
            handle_namer: Arc::new(crate::handle_naming::HandleNamer::default()),
            artifacts,
            formatter,
            lsp,
            bus,
            summarizer: None,
            handoff_options: TerminalHandoffOptions::default(),
            rewriter_keep_recent: CompactionConfig::default().keep_recent,
            token_accounting: TokenAccounting::default(),
            rewriters,
            native_hooks,
            hooks: None,
            session_bundle_hooks: Arc::new(RwLock::new(HashMap::new())),
            session_channel_policies,
            mail_steer_suppressed,
            staged_mail_acks,
            governor: None,
            sidecar_environment: None,
            reviver,
            runtime_owner,
            turn_gate,
            round_retry_waits: &crate::kernel::ROUND_RETRY_WAITS,
            #[cfg(test)]
            mail_pre_append_gate: None,
        };
        let store_for_activity = engine.store.clone();
        let gate_for_activity = Arc::clone(&engine.turn_gate);
        let active = Arc::new(move |session| gate_for_activity.is_active(session));
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(crate::project_activity::serve(
                store_for_activity,
                active,
                activity_rx,
            ));
        }
        engine
    }

    /// Record the id claimed with `SessionStore::claim_runtime_owner`.
    #[must_use]
    pub fn with_runtime_owner(mut self, owner: hya_proto::OwnerRunId) -> Self {
        self.runtime_owner = owner;
        self
    }
    /// The owner identity associated with this engine's store claim.
    #[must_use]
    pub fn runtime_owner(&self) -> hya_proto::OwnerRunId {
        self.runtime_owner
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_mail_pre_append_gate(mut self, gate: MailPreAppendGate) -> Self {
        self.mail_pre_append_gate = Some(Arc::new(gate));
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

    /// Skip external project instructions when recording a new session's context.
    #[must_use]
    pub fn with_pure_context(mut self, pure: bool) -> Self {
        self.pure_context = pure;
        self
    }

    /// Set the host prompt retained when a catalog agent omits its own prompt.
    /// Applies to resident revival and autonomous main reconstruction.
    #[must_use]
    pub fn with_base_system_prompt(mut self, prompt: String) -> Self {
        self.base_system_prompt = prompt;
        self
    }

    pub(crate) fn base_system_prompt(&self) -> &str {
        &self.base_system_prompt
    }

    /// Install the process-wide reasoning fallback.
    #[must_use]
    pub fn with_global_reasoning(mut self, reasoning: Option<ReasoningEffort>) -> Self {
        self.global_reasoning = reasoning;
        self
    }

    /// Offer only `tool` to the model of every agent whose view holds it.
    ///
    /// The agent's other tools stay executable (a tool reaches them through
    /// [`hya_tool::ToolCtx::nested`]) and are described inside `tool`'s
    /// description. A subagent's `report` stays offered. An agent without
    /// `tool` in its view is unaffected.
    #[must_use]
    pub fn with_exclusive_tool(mut self, tool: Option<String>) -> Self {
        self.exclusive_tool = tool.map(Arc::from);
        self
    }

    /// Install ABI v2 native bundle hooks (preserved across [`Self::with_hooks`]).
    #[must_use]
    pub fn with_native_hooks(mut self, hooks: Arc<dyn HookDispatcher>) -> Self {
        self.native_hooks = Some(hooks);
        self
    }

    /// Install plugin/host hooks for command/tool/chat interception and events.
    #[must_use]
    pub fn with_hooks(mut self, hooks: Arc<dyn HookDispatcher>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Install ABI v2 native hooks registered by enabled native bundles.
    ///
    /// Replaces any default native hooks loaded by [`Self::try_new`] so
    /// disabling a native bundle cleanly removes its hooks.
    #[must_use]
    pub fn with_native_hook_list(
        mut self,
        hooks: Vec<Arc<dyn hya_runtime_abi::NativeHook>>,
    ) -> Self {
        self.native_hooks = (!hooks.is_empty()).then(|| {
            Arc::new(crate::hooks::NativeHookDispatcher::new(hooks)) as Arc<dyn HookDispatcher>
        });
        self
    }

    /// Replace dynamic URI scheme handlers on this engine's [`ArtifactPlane`]
    /// with the given ABI v2 [`hya_runtime_abi::UriSchemaHandler`] list.
    ///
    /// Passing an empty list clears any default URI handlers (e.g. when
    /// `hya/agent-channels` is explicitly disabled).
    #[must_use]
    pub fn with_uri_schema_handlers(
        mut self,
        handlers: Vec<Arc<dyn hya_runtime_abi::UriSchemaHandler>>,
        runtime: Arc<dyn hya_runtime_abi::HostRuntime>,
    ) -> Self {
        let mut artifacts = self.artifacts.clear_uri_handlers();
        for handler in handlers {
            let adapted =
                hya_runtime_abi::adapt_uri_schema_handler(handler, Some(Arc::clone(&runtime)));
            match artifacts.clone().with_uri_handler(adapted) {
                Ok(updated) => artifacts = updated,
                Err(error) => {
                    tracing::warn!(%error, "skipping duplicate or protected ABI v2 URI handler");
                }
            }
        }
        self.artifacts = artifacts;
        self
    }

    /// Suppress or re-enable in-turn unread-mail steering for `session`.
    pub fn set_mail_steer_suppressed(&self, session: SessionId, suppressed: bool) {
        let mut map = self
            .mail_steer_suppressed
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if suppressed {
            map.insert(session, true);
        } else {
            map.remove(&session);
        }
    }

    /// Whether in-turn unread-mail steering is currently suppressed for `session`.
    #[must_use]
    pub fn is_mail_steer_suppressed(&self, session: SessionId) -> bool {
        self.mail_steer_suppressed
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session)
            .copied()
            .unwrap_or(false)
    }

    /// Stage a `mail_consumed` acknowledgement from `UnreadMailSteerHook` to
    /// commit atomically with the current tool result on `session`.
    pub fn stage_mail_steer_ack(&self, session: SessionId, ack: hya_runtime_abi::StagedMailAck) {
        self.staged_mail_acks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session, ack);
    }

    /// Take and clear any staged `mail_consumed` acknowledgement for `session`.
    #[must_use]
    pub fn take_staged_mail_ack(
        &self,
        session: SessionId,
    ) -> Option<hya_runtime_abi::StagedMailAck> {
        self.staged_mail_acks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session)
    }

    /// Return `true` when `handle` under `root` is currently stopped by the user.
    #[must_use]
    pub fn is_member_stopped_by_user(&self, root: SessionId, handle: &str) -> bool {
        self.archive_reviver()
            .is_some_and(|reviver| reviver.stopped_by_user(root, handle))
    }

    /// Revive an archived direct child member and deliver its pending DM mail.
    ///
    /// # Errors
    /// Returns [`CoreError`] when no resident supervisor is attached or revival fails.
    pub async fn revive_archived_member_with_mail(
        &self,
        request: hya_runtime_abi::MemberMailRevivalRequest,
    ) -> Result<usize, CoreError> {
        let Some(supervisor) = self.resident_supervisor() else {
            return Err(CoreError::MailRevivalRequired {
                sender: request.from_handle,
                channel: request.channel,
                target: request.target_handle,
            });
        };
        let policy = self.session_channel_policy(request.from_session);
        supervisor
            .revive_episode(
                request.root,
                &request.from_handle,
                &request.target_handle,
                request.channel,
                request.kind,
                request.body,
                request.actor_claim.as_ref(),
                policy,
            )
            .await
    }

    /// Build a lightweight [`hya_runtime_abi::HostRuntime`] backed by this
    /// engine's store, bus, turn gate, and reviver handles.
    #[must_use]
    pub(crate) fn channel_host_runtime(&self) -> Arc<dyn hya_runtime_abi::HostRuntime> {
        Arc::new(channel_host::EngineChannelsHostRuntime {
            store: self.store.clone(),
            bus: self.bus.clone(),
            runtime_owner: self.runtime_owner,
            session_channel_policies: Arc::clone(&self.session_channel_policies),
            mail_steer_suppressed: Arc::clone(&self.mail_steer_suppressed),
            staged_mail_acks: Arc::clone(&self.staged_mail_acks),
            reviver: Arc::clone(&self.reviver),
            turn_gate: Arc::clone(&self.turn_gate),
        })
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

    /// Set the scope overlay cache limits (default: 32 scopes, 30 minutes
    /// idle). Shared with every clone of this engine.
    #[must_use]
    pub fn with_catalog_scope_cache(self, config: scope_binding::CatalogScopeCacheConfig) -> Self {
        self.set_catalog_scope_cache_config(config);
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

    /// Register started ABI v2 [`NativeService`](hya_runtime_abi::NativeService) instances
    /// so session busy, cancellation, and delete-idle coordination include them.
    pub fn register_native_services(&self, services: Vec<Arc<dyn hya_runtime_abi::NativeService>>) {
        self.native_services
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(services);
    }

    /// Register ABI v2 [`ProjectionReducer`](hya_runtime_abi::ProjectionReducer)
    /// instances on the underlying event store.
    pub fn register_native_projection_reducers(
        &self,
        reducers: Vec<Arc<dyn hya_runtime_abi::ProjectionReducer>>,
    ) {
        for reducer in reducers {
            self.store
                .register_projection_reducer(Arc::new(AbiV2ProjectionReducerAdapter {
                    inner: reducer,
                }));
        }
    }

    /// Return `true` if any registered native service owns active background work on `session`.
    #[must_use]
    pub fn has_active_service_work(&self, session: SessionId) -> bool {
        self.native_services
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|service| service.is_session_busy(session))
    }

    /// Cancel active background work on `session` across all registered native services.
    #[must_use]
    pub fn cancel_service_work(&self, session: SessionId) -> bool {
        let services = self
            .native_services
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut cancelled = false;
        for service in services {
            cancelled |= service.cancel_session(session);
        }
        cancelled
    }

    /// Wait until all registered native services finish any active work on `session`.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] if a native service reports a wait error.
    pub async fn wait_service_work_idle(&self, session: SessionId) -> Result<(), CoreError> {
        let services = self
            .native_services
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for service in services {
            service
                .wait_session_idle(session)
                .await
                .map_err(|error| CoreError::Invalid(error.to_string()))?;
        }
        Ok(())
    }

    /// Inject the mailbox plane whose service loop this engine drives (see
    /// [`run_mailbox_service`](crate::mailbox::run_mailbox_service)). Wired from
    /// the app layer alongside the spawner, mirroring the established plane
    /// pattern so `hya-tool` stays free of a `hya-engine` dependency.
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

    /// Whether a lifecycle service runs: without one no member can `report`.
    pub(crate) fn lifecycle_connected(&self) -> bool {
        self.lifecycle.is_connected()
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
    /// (live or archived) nor one this process currently reserves.
    /// Caller serializes this snapshot with registration commits.
    pub(crate) async fn mint_member_leaf(
        &self,
        root: SessionId,
        prefix: &str,
    ) -> Result<crate::handle_naming::HandleReservation, CoreError> {
        let projection = self.read_projection_shared(root).await?;
        let taken = crate::handle_naming::team_leaves(&projection);
        Ok(self.handle_namer.reserve(root, prefix, taken))
    }

    #[cfg(test)]
    pub(crate) fn pending_member_names(&self, root: SessionId) -> usize {
        self.handle_namer.reserved_count(root)
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

    /// Set both the terminal handoff summarizer and rewriter retention.
    ///
    /// Use [`Self::with_terminal_handoff`] and [`Self::with_rewriter_keep_recent`]
    /// to configure the two independently.
    #[must_use]
    pub fn with_compaction(
        self,
        summarizer: Arc<dyn Summarizer>,
        config: CompactionConfig,
    ) -> Self {
        self.with_terminal_handoff(
            summarizer,
            TerminalHandoffOptions {
                max_output_tokens: config.summary_max_tokens,
            },
        )
        .with_rewriter_keep_recent(config.keep_recent)
    }

    /// Set the terminal handoff generator and its output budget.
    #[must_use]
    pub fn with_terminal_handoff(
        mut self,
        summarizer: Arc<dyn Summarizer>,
        options: TerminalHandoffOptions,
    ) -> Self {
        self.summarizer = Some(summarizer);
        self.handoff_options = options;
        self
    }

    /// Keep tool outputs from this many recent messages when shake runs.
    #[must_use]
    pub fn with_rewriter_keep_recent(mut self, keep_recent: usize) -> Self {
        self.rewriter_keep_recent = keep_recent;
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

    /// Override the per-retry backoff schedule for provider round retries.
    ///
    /// Defaults to [`crate::kernel::ROUND_RETRY_WAITS`].
    #[must_use]
    pub fn with_round_retry_waits(mut self, waits: &'static [Duration]) -> Self {
        self.round_retry_waits = waits;
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

    /// Process permission plane. Remembered grants installed on it are shared
    /// with every session plane derived from it.
    #[must_use]
    pub fn permission_plane(&self) -> &PermissionPlane {
        &self.permission
    }

    /// Formatter plane used after write/edit/patch.
    #[must_use]
    pub fn formatter(&self) -> &FormatterPlane {
        &self.formatter
    }
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

/// The rewriters registered by internal bundles, in registration order.
/// Successful registrations are shared process-wide.
type NativeRewriters = Arc<[Arc<dyn hya_context::rewrite::Rewriter>]>;
type NativeProjectionReducers = Vec<Arc<dyn hya_runtime_abi::ProjectionReducer>>;

fn internal_bundles() -> Result<(NativeRewriters, NativeProjectionReducers), CoreError> {
    static BUNDLES: std::sync::Mutex<Option<(NativeRewriters, NativeProjectionReducers)>> =
        std::sync::Mutex::new(None);
    let mut slot = BUNDLES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((rewriters, reducers)) = slot.as_ref() {
        return Ok((Arc::clone(rewriters), reducers.clone()));
    }
    let regs = hya_core::bundles::load_internal()?;
    let rewriters: NativeRewriters = regs.rewriters.into();
    let reducers = regs.projection_reducers;
    *slot = Some((Arc::clone(&rewriters), reducers.clone()));
    Ok((rewriters, reducers))
}

/// Bridges an ABI v2 native bundle [`hya_runtime_abi::ProjectionReducer`] onto
/// the host's [`hya_proto::ProjectionReducer`] pipeline.
struct AbiV2ProjectionReducerAdapter {
    inner: Arc<dyn hya_runtime_abi::ProjectionReducer>,
}

impl hya_proto::ProjectionReducer for AbiV2ProjectionReducerAdapter {
    fn bundle_id(&self) -> &str {
        self.inner.bundle_id()
    }

    fn fold_extension(
        &self,
        projection: &mut hya_proto::Projection,
        session: SessionId,
        kind: &str,
        payload: &Value,
    ) {
        let bundle_id = self.inner.bundle_id();
        let state = projection
            .extensions
            .entry(bundle_id.to_string())
            .or_insert_with(|| self.inner.initial_state());
        let ctx = hya_runtime_abi::ExtensionEventContext {
            session,
            seq: hya_proto::EventSeq(projection.last_seq),
            ts_millis: 0,
            bundle: bundle_id.to_string(),
            kind: kind.to_string(),
            payload: payload.clone(),
        };
        if self.inner.apply(state, &ctx).is_err() {
            return;
        }
        // Mirror compaction tool-output eviction notices onto PartProjection::Tool.
        if bundle_id == hya_tool::extension::BUNDLE_COMPACTION
            && kind == "context_evicted"
            && let Some(parts) = payload.get("parts").and_then(Value::as_array)
        {
            for entry in parts {
                if let (Some(part_id), Some(notice)) = (
                    entry
                        .get("part")
                        .and_then(|v| serde_json::from_value::<hya_proto::PartId>(v.clone()).ok()),
                    entry.get("notice").and_then(Value::as_str),
                ) && let Some(hya_proto::PartProjection::Tool {
                    eviction_notice, ..
                }) = projection
                    .session
                    .messages
                    .iter_mut()
                    .rev()
                    .flat_map(|row| row.parts.iter_mut())
                    .find(|part| part.id() == part_id)
                {
                    *eviction_notice = Some(notice.to_string());
                }
            }
        }
        // Bridge `hya/agent-channels` mail events with `hya/multi-agent` roster state
        // (cursor synchronization and terminal-resident inbox filtering) until CHR-30.
        if bundle_id == hya_tool::extension::BUNDLE_CHANNELS {
            if kind == "mail_consumed"
                && let (Some(handle), Some(through)) = (
                    payload.get("handle").and_then(Value::as_str),
                    payload.get("through").and_then(Value::as_u64),
                )
                && let Some(multi_val) = projection
                    .extensions
                    .get(hya_tool::extension::BUNDLE_MULTI_AGENT)
                && let Ok(mut multi) = serde_json::from_value::<
                    hya_tool::extension::MultiAgentExtensionState,
                >(multi_val.clone())
                && let Some(entry) = multi.roster.get_mut(handle)
            {
                entry.resident_cursor = entry.resident_cursor.max(through);
                if let Ok(serialized) = serde_json::to_value(&multi) {
                    projection.extensions.insert(
                        hya_tool::extension::BUNDLE_MULTI_AGENT.to_string(),
                        serialized,
                    );
                }
            } else if kind == "mail_sent"
                && let Some(multi_val) = projection
                    .extensions
                    .get(hya_tool::extension::BUNDLE_MULTI_AGENT)
                && let Ok(multi) = serde_json::from_value::<
                    hya_tool::extension::MultiAgentExtensionState,
                >(multi_val.clone())
                && let Some(channels_val) = projection
                    .extensions
                    .get(hya_tool::extension::BUNDLE_CHANNELS)
                && let Ok(mut channels) = serde_json::from_value::<
                    hya_tool::extension::ChannelsExtensionState,
                >(channels_val.clone())
                && let Some(last_idx) = channels.mail.len().checked_sub(1)
                && matches!(
                    channels.mail[last_idx].to,
                    hya_proto::MailEndpoint::Channel(_)
                )
            {
                let mut pruned = false;
                for (member, inbox) in &mut channels.inboxes {
                    let is_live = multi
                        .roster
                        .get(member)
                        .is_none_or(hya_tool::extension::RosterEntry::accepts_mail);
                    if !is_live && inbox.last() == Some(&last_idx) {
                        inbox.pop();
                        pruned = true;
                    }
                }
                if pruned && let Ok(serialized) = serde_json::to_value(&channels) {
                    projection
                        .extensions
                        .insert(hya_tool::extension::BUNDLE_CHANNELS.to_string(), serialized);
                }
            }
        }
    }
}

/// Default native hooks registered by first-party ABI v2 native tool bundles
/// (`hya/plan`, etc.), cached process-wide for [`SessionEngine::try_new`].
/// Callers with explicit [`hya_bundle::BuiltinBundleSelection`] override this
/// via [`SessionEngine::with_native_hook_list`].
fn default_native_hook_list() -> Vec<Arc<dyn hya_runtime_abi::NativeHook>> {
    static HOOKS: std::sync::Mutex<Option<Vec<Arc<dyn hya_runtime_abi::NativeHook>>>> =
        std::sync::Mutex::new(None);
    let mut slot = HOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cached) = slot.as_ref() {
        return cached.clone();
    }
    let mut hooks = Vec::new();
    for identity in hya_tool_loader::NATIVE_V2_TOOL_BUNDLES {
        match hya_tool_loader::load_v2(identity, hya_tool_loader::noop_host_runtime()) {
            Ok(regs) => hooks.extend(regs.hooks),
            Err(error) => {
                tracing::debug!(
                    bundle = identity,
                    %error,
                    "default native v2 hooks unavailable"
                );
            }
        }
    }
    *slot = Some(hooks.clone());
    hooks
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
        // Anchoring and output budget are per-call, not per-definition.
        previous_summary: None,
        max_output_tokens: None,
        // Callers that bill a session attach their own collector.
        usage: None,
    }
}
