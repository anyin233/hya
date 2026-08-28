//! Backend runtime bootstrap: config → store → session engine → optional HTTP router.
//!
//! Call [`resolve_runtime`] (or build a [`RuntimeConfig`] offline), open a store with
//! [`open_store`], assemble a [`BuiltSessionEngine`] via [`build_session_engine`], or
//! use [`HyaRuntime::start`] for the full server-ready process path.

// allow: SIZE_OK — reviewed Phase 1 keeps backend bootstrap glue in this public API module.
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use hya_bundle::{BundleCatalog, SpawnLifecycle};
use hya_core::agent_catalog::{AgentCatalog, AgentDefinition};
use hya_core::{
    AdmissionMemberIdentity, AgentResourcePolicy, AgentSpec, BoundSidecarFactory,
    BoundSpawnRequest, BoundSpawnSender, BoundWorkflowRequest, BoundWorkflowSender,
    CategoryRegistry, CompactionConfig, CoreError, CreateSession, EventBus, MemberEvidence,
    MemberSpec, MemberStatus, ModelSummarizer, OperationReservation, PromptEnv, ResidentSupervisor,
    RuntimeRegistry, SessionEngine, SidecarEnvironment, SidecarHandle, SidecarLifecycle,
    SidecarStart, SpawnAdmissionOutcome, SubagentGovernor, Summarizer, TeamEvidenceEnvelope,
    TurnBinding, build_system_prompt, discover_workflow_files, load_workflow_by_name,
    load_workflow_file, project_envelope, project_envelope_for_actor, run_mailbox_service,
    run_pre_admitted_member, run_pre_admitted_team, run_pre_admitted_team_for_actor, run_workflow,
};

// Single discovery/date implementation lives in hya-core; re-export for callers.
pub use hya_core::{discover_context_files, today};
use hya_mcp::McpServerConfig;
use hya_plugin::HostInfo;
use hya_plugin::client::{ChildGuard, PluginClient};
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{
    ActivationLifecycle, ActivationMetadata, HookName, HookRegistration, PluginKindWire, ToolInfo,
};
use hya_proto::{
    AgentName, MemberId, ModelRef, OwnerRunId, SessionId, SubagentMode, ToolName, ToolSchema,
};
use hya_provider::{DevProvider, ProviderRouter, ReasoningEffort};
use hya_store::{
    AdmissionBatchClaimOutcome, AdmissionClaim, AdmissionIntent, AdmissionLaunch,
    AdmissionTerminal, SessionStore, StoreError,
};
use hya_tool::{
    Action, AskRequest, InteractionPlane, InvocationPolicy, MailboxPlane, MemberOutcome, Mode,
    PermissionModel, PermissionPlane, PermissionRules, QuestionRequest, ResolvedTool, Resource,
    Rule, SpawnError, SpawnMember, SpawnRequest, Tool, ToolCtx, ToolError, ToolPermission,
    ToolRegistry, WebSearchConfig, WebSearchPlane,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::OpenOptions;
use std::io::Write;

use crate::config;
use crate::runtime_reconcile::{
    DesiredSource, PreparedFailure, PreparedResult, RuntimeMcpControl, RuntimeReconciler, SourceId,
    prepare_desired_source, prepared_plugin_source,
};
use crate::spawn_intent::{PriorStartV1, SpawnIntentInputV1, SpawnIntentV1};
use crate::{InstalledBundleRefresh, bundle_registry_path, formatter_config, plugins};

/// Agent catalog for a process with no installed bundles yet.
///
/// Built-in agents are compiled into the binary (`hya_core::builtin_agents`),
/// so a fresh install starts with an empty installed-bundle catalog and still
/// resolves every built-in. `from_verified_catalogs(&[])` (not `from_prepared`)
/// is deliberate: it records verified provenance for the empty set, which keeps
/// the runtime semantic fingerprint available.
pub fn builtin_agent_catalog() -> anyhow::Result<Arc<AgentCatalog>> {
    let bundles = BundleCatalog::from_verified_catalogs(&[])
        .context("build empty installed AgentBundle catalog")?;
    let catalog =
        AgentCatalog::new(Arc::new(bundles)).context("build agent catalog over built-ins")?;
    Ok(Arc::new(catalog))
}

/// Host identity sent to plugins during `initialize` (`name` + crate version).
pub fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// Dev/offline provider router and model id when no live config is usable.
///
/// Returns a router containing only `DevProvider` and either `model_override`
/// or the literal model id `offline`.
pub fn offline_router(model_override: Option<String>) -> (ProviderRouter, String) {
    let router = ProviderRouter::new().with(Arc::new(DevProvider::new()));
    (
        router,
        model_override.unwrap_or_else(|| "offline".to_string()),
    )
}

fn process_owner_run_id() -> OwnerRunId {
    static OWNER_RUN_ID: OnceLock<OwnerRunId> = OnceLock::new();
    *OWNER_RUN_ID.get_or_init(OwnerRunId::new)
}

/// Compaction thresholds from env (`HYA_COMPACTION_*`) or [`CompactionConfig`] defaults.
pub fn compaction_config() -> CompactionConfig {
    let default = CompactionConfig::default();
    let token_threshold = std::env::var("HYA_COMPACTION_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default.token_threshold);
    let keep_recent = std::env::var("HYA_COMPACTION_KEEP_RECENT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default.keep_recent);
    let context_fraction = std::env::var("HYA_COMPACTION_CONTEXT_FRACTION")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default.context_fraction);
    CompactionConfig {
        token_threshold,
        keep_recent,
        context_fraction,
    }
}

/// Canonical Harness agent base string (no Environment / AGENTS).
pub const HARNESS_AGENT_BASE: &str = "You are hya, a coding agent.";

pub(crate) fn materialize_bundle_sidecar_resources(
    binding: &TurnBinding,
    stable_agent_id: &str,
    activation_dir: &Path,
) -> Result<Vec<PathBuf>, CoreError> {
    let (bundle_id, _) = binding
        .bundle_catalog()
        .resolve_agent_entry(stable_agent_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_agent_id.to_string(),
        })?;

    binding.has_selected_bundle_sidecar_capability(stable_agent_id)?;
    let policy = binding.agent_resource_policy(stable_agent_id)?;
    let catalog = binding.bundle_catalog();
    let mut resources = BTreeMap::new();
    let mut entrypoints = BTreeMap::new();

    for (kind, selected_ids) in [
        (
            hya_bundle::ExportKind::Tool,
            policy.selected_bundle_tool_ids(),
        ),
        (hya_bundle::ExportKind::Hook, policy.canonical_hook_ids()),
    ] {
        for stable_id in selected_ids {
            let (owner_bundle_id, resource) =
                catalog.resolve_resource_entry(bundle_id, kind, stable_id)?;
            let extensions = catalog
                .bundle_resources(owner_bundle_id, hya_bundle::ExportKind::Extension)
                .ok_or_else(|| {
                    CoreError::Invalid(format!(
                        "bundle extension catalog missing `{owner_bundle_id}`"
                    ))
                })?;
            let matches = extensions
                .iter()
                .filter(|extension| extension.source_path == resource.source_path)
                .collect::<Vec<_>>();
            let [extension] = matches.as_slice() else {
                return Err(CoreError::Invalid(format!(
                    "expected exactly one extension for bundle resource `{}`",
                    resource.stable_id
                )));
            };
            insert_materialized_resource(&mut resources, owner_bundle_id, resource)?;
            insert_materialized_resource(&mut resources, owner_bundle_id, extension)?;
            entrypoints
                .entry((owner_bundle_id.to_string(), extension.stable_id.clone()))
                .or_insert_with(|| extension.source_path.clone());
        }
    }

    let owner_ids = resources
        .keys()
        .map(|(owner_bundle_id, _)| owner_bundle_id.as_str())
        .collect::<BTreeSet<_>>();
    let owner_slots = (owner_ids.len() > 1).then(|| {
        owner_ids
            .iter()
            .enumerate()
            .map(|(index, owner_bundle_id)| {
                ((*owner_bundle_id).to_string(), format!("owner-{index:04}"))
            })
            .collect::<BTreeMap<_, _>>()
    });

    for ((owner_bundle_id, source_path), resource) in resources {
        let relative_path = if let Some(owner_slots) = &owner_slots {
            let Some(slot) = owner_slots.get(&owner_bundle_id) else {
                return Err(CoreError::Invalid(format!(
                    "missing materialization slot for bundle `{owner_bundle_id}`"
                )));
            };
            PathBuf::from(slot).join(&source_path)
        } else {
            PathBuf::from(&source_path)
        };
        let output_path = activation_dir.join(&relative_path);
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                CoreError::Invalid(format!(
                    "create bundle resource directory `{}`: {error}",
                    parent.display()
                ))
            })?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)
            .map_err(|error| {
                CoreError::Invalid(format!(
                    "create bundle resource `{}`: {error}",
                    output_path.display()
                ))
            })?;
        output
            .write_all(resource.content.as_bytes())
            .map_err(|error| {
                CoreError::Invalid(format!(
                    "write bundle resource `{}`: {error}",
                    output_path.display()
                ))
            })?;
    }

    let mut extension_paths = Vec::with_capacity(entrypoints.len());
    for ((owner_bundle_id, _), source_path) in entrypoints {
        let output_path = if let Some(owner_slots) = &owner_slots {
            let Some(slot) = owner_slots.get(&owner_bundle_id) else {
                return Err(CoreError::Invalid(format!(
                    "missing materialization slot for bundle `{owner_bundle_id}`"
                )));
            };
            activation_dir.join(slot).join(source_path)
        } else {
            activation_dir.join(source_path)
        };
        extension_paths.push(output_path);
    }
    Ok(extension_paths)
}

fn insert_materialized_resource<'a>(
    resources: &mut BTreeMap<(String, String), &'a hya_bundle::PreparedResource>,
    owner_bundle_id: &str,
    resource: &'a hya_bundle::PreparedResource,
) -> Result<(), CoreError> {
    validate_materialized_resource_path(&resource.source_path)?;
    match resources.entry((owner_bundle_id.to_string(), resource.source_path.clone())) {
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(resource);
        }
        std::collections::btree_map::Entry::Occupied(mut entry) => {
            let existing = entry.get_mut();
            if existing.digest != resource.digest || existing.content != resource.content {
                return Err(CoreError::Invalid(format!(
                    "conflicting bundle resource path `{}`",
                    resource.source_path
                )));
            }
        }
    }
    Ok(())
}

fn validate_materialized_resource_path(source_path: &str) -> Result<(), CoreError> {
    let mut segments = source_path.split('/');
    let Some(first) = segments.next() else {
        return Err(CoreError::Invalid("empty bundle resource path".to_string()));
    };
    if first.is_empty()
        || first == "."
        || first == ".."
        || (first.len() == 2
            && first
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
            && first.as_bytes().get(1) == Some(&b':'))
    {
        return Err(CoreError::Invalid(format!(
            "unsafe bundle resource path `{source_path}`"
        )));
    }
    if source_path.contains('\\') || source_path.as_bytes().contains(&0) {
        return Err(CoreError::Invalid(format!(
            "unsafe bundle resource path `{source_path}`"
        )));
    }
    for segment in segments {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(CoreError::Invalid(format!(
                "unsafe bundle resource path `{source_path}`"
            )));
        }
    }
    if !Path::new(source_path)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(CoreError::Invalid(format!(
            "unsafe bundle resource path `{source_path}`"
        )));
    }
    Ok(())
}

struct BundleSidecarEnvironment {
    command: Option<Vec<String>>,
    staging_root: PathBuf,
    #[cfg(test)]
    terminate_notify: Option<Arc<tokio::sync::Notify>>,
    #[cfg(test)]
    test_observer: Option<Arc<AdmissionTestObserver>>,
    #[cfg(test)]
    uniform_probe: Option<Arc<ForegroundHandlerUniformProbe>>,
}

#[cfg(test)]
struct AdmissionTestObserver {
    sequence: std::sync::atomic::AtomicUsize,
    owner_operation_ids: std::sync::Mutex<Vec<hya_proto::OperationId>>,
    resolution_targets: std::sync::Mutex<Vec<String>>,
    foreign_wake: tokio::sync::Notify,
    foreign_wake_operation_ids: std::sync::Mutex<Vec<hya_proto::OperationId>>,
    cleanup_attempt: tokio::sync::Notify,
    cleanup_finished: tokio::sync::Notify,
}

#[cfg(test)]
impl AdmissionTestObserver {
    fn new() -> Self {
        Self {
            sequence: std::sync::atomic::AtomicUsize::new(0),
            owner_operation_ids: std::sync::Mutex::new(Vec::new()),
            resolution_targets: std::sync::Mutex::new(Vec::new()),
            foreign_wake: tokio::sync::Notify::new(),
            foreign_wake_operation_ids: std::sync::Mutex::new(Vec::new()),
            cleanup_attempt: tokio::sync::Notify::new(),
            cleanup_finished: tokio::sync::Notify::new(),
        }
    }

    fn mark_step(&self) {
        self.sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn mark_owner_acquired(&self, operation_id: hya_proto::OperationId) {
        self.mark_step();
        self.owner_operation_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(operation_id);
    }

    fn mark_resolution_hook(&self, stable_agent_id: &str) {
        self.mark_step();
        self.resolution_targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(stable_agent_id.to_string());
    }

    fn mark_cleanup_attempt(&self) {
        self.cleanup_attempt.notify_one();
    }

    fn mark_cleanup_finished(&self) {
        self.cleanup_finished.notify_one();
    }
}

struct ForegroundAdmissionWake {
    _generation: u64,
}

struct ForegroundAdmissionWakeRoute {
    generation: u64,
    sender: tokio::sync::mpsc::Sender<ForegroundAdmissionWake>,
}

struct ForegroundAdmissionWakeRouter {
    routes: std::sync::Mutex<BTreeMap<hya_proto::OperationId, ForegroundAdmissionWakeRoute>>,
    next_generation: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    test_observer: Option<Arc<AdmissionTestObserver>>,
}

struct ForegroundAdmissionWakeRegistration {
    operation_id: hya_proto::OperationId,
    generation: u64,
    router: Arc<ForegroundAdmissionWakeRouter>,
}

impl ForegroundAdmissionWakeRouter {
    fn new(#[cfg(test)] test_observer: Option<Arc<AdmissionTestObserver>>) -> Self {
        Self {
            routes: std::sync::Mutex::new(BTreeMap::new()),
            next_generation: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            test_observer,
        }
    }

    fn register(
        self: &Arc<Self>,
        operation_id: hya_proto::OperationId,
    ) -> Option<(
        ForegroundAdmissionWakeRegistration,
        tokio::sync::mpsc::Receiver<ForegroundAdmissionWake>,
    )> {
        let generation = self
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let mut routes = self.routes.lock().ok()?;
        if routes.len() >= FOREGROUND_HANDLER_CAP || routes.contains_key(&operation_id) {
            return None;
        }
        routes.insert(
            operation_id,
            ForegroundAdmissionWakeRoute { generation, sender },
        );
        Some((
            ForegroundAdmissionWakeRegistration {
                operation_id,
                generation,
                router: Arc::clone(self),
            },
            receiver,
        ))
    }

    fn wake(&self, operation_id: hya_proto::OperationId) {
        let Some((sender, generation)) = self.routes.lock().ok().and_then(|routes| {
            routes
                .get(&operation_id)
                .map(|route| (route.sender.clone(), route.generation))
        }) else {
            return;
        };
        if sender
            .try_send(ForegroundAdmissionWake {
                _generation: generation,
            })
            .is_ok()
        {
            #[cfg(test)]
            if let Some(observer) = &self.test_observer {
                observer
                    .foreign_wake_operation_ids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(operation_id);
                observer.foreign_wake.notify_one();
            }
        }
    }
}

impl Drop for ForegroundAdmissionWakeRegistration {
    fn drop(&mut self) {
        if let Ok(mut routes) = self.router.routes.lock()
            && routes
                .get(&self.operation_id)
                .is_some_and(|route| route.generation == self.generation)
        {
            routes.remove(&self.operation_id);
        }
    }
}

#[cfg(test)]
struct ForegroundHandlerUniformProbe {
    prepare_entered: tokio::sync::Notify,
    prepare_release: tokio::sync::Notify,
    before_claim: tokio::sync::Notify,
    before_claim_release: tokio::sync::Notify,
    after_claim: tokio::sync::Notify,
    after_claim_release: tokio::sync::Notify,
    owner_run_entered: tokio::sync::Notify,
    owner_run_release: tokio::sync::Notify,
    supervisor_handler_active: std::sync::atomic::AtomicUsize,
    supervisor_handler_owned: std::sync::atomic::AtomicUsize,
    max_handler_live: std::sync::atomic::AtomicUsize,
    reply_owners: std::sync::atomic::AtomicUsize,
    detached_postclaim_owner_spawns: std::sync::atomic::AtomicUsize,
    real_member_task_installations: std::sync::atomic::AtomicUsize,
    supervisor_full_observed: tokio::sync::Semaphore,
    handler_acquisitions: tokio::sync::Semaphore,
    preparation_acquisitions: tokio::sync::Semaphore,
    handler_releases: tokio::sync::Semaphore,
    preparation_entries: std::sync::atomic::AtomicUsize,
    watched_preparation_operation: std::sync::Mutex<Option<hya_proto::OperationId>>,
    watched_preparation_entered: tokio::sync::Notify,
    watched_preparation_release: tokio::sync::Notify,
}

#[cfg(test)]
impl ForegroundHandlerUniformProbe {
    fn new() -> Self {
        Self {
            prepare_entered: tokio::sync::Notify::new(),
            prepare_release: tokio::sync::Notify::new(),
            before_claim: tokio::sync::Notify::new(),
            before_claim_release: tokio::sync::Notify::new(),
            after_claim: tokio::sync::Notify::new(),
            after_claim_release: tokio::sync::Notify::new(),
            owner_run_entered: tokio::sync::Notify::new(),
            owner_run_release: tokio::sync::Notify::new(),
            supervisor_handler_active: std::sync::atomic::AtomicUsize::new(0),
            supervisor_handler_owned: std::sync::atomic::AtomicUsize::new(0),
            max_handler_live: std::sync::atomic::AtomicUsize::new(0),
            reply_owners: std::sync::atomic::AtomicUsize::new(0),
            detached_postclaim_owner_spawns: std::sync::atomic::AtomicUsize::new(0),
            real_member_task_installations: std::sync::atomic::AtomicUsize::new(0),
            supervisor_full_observed: tokio::sync::Semaphore::new(0),
            handler_acquisitions: tokio::sync::Semaphore::new(0),
            preparation_acquisitions: tokio::sync::Semaphore::new(0),
            handler_releases: tokio::sync::Semaphore::new(0),
            preparation_entries: std::sync::atomic::AtomicUsize::new(0),
            watched_preparation_operation: std::sync::Mutex::new(None),
            watched_preparation_entered: tokio::sync::Notify::new(),
            watched_preparation_release: tokio::sync::Notify::new(),
        }
    }

    fn watch_preparation(&self, operation: hya_proto::OperationId) {
        let mut watched = self
            .watched_preparation_operation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *watched = Some(operation);
    }

    fn mark_preparation_entered(&self, operation: hya_proto::OperationId) -> bool {
        self.preparation_entries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let watched = self
            .watched_preparation_operation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|watched| watched == operation);
        if watched {
            self.watched_preparation_entered.notify_one();
        }
        watched
    }
}

#[cfg(test)]
struct ForegroundHandlerProbeGuard {
    probe: Arc<ForegroundHandlerUniformProbe>,
}

#[cfg(test)]
impl ForegroundHandlerProbeGuard {
    fn new(probe: Arc<ForegroundHandlerUniformProbe>) -> Self {
        let live = probe
            .supervisor_handler_active
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        probe
            .supervisor_handler_owned
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        probe
            .max_handler_live
            .fetch_max(live, std::sync::atomic::Ordering::SeqCst);
        probe
            .reply_owners
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        probe.handler_acquisitions.add_permits(1);
        Self { probe }
    }
}

#[cfg(test)]
impl Drop for ForegroundHandlerProbeGuard {
    fn drop(&mut self) {
        self.probe
            .supervisor_handler_active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.probe
            .supervisor_handler_owned
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.probe
            .reply_owners
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.probe.handler_releases.add_permits(1);
    }
}

impl BundleSidecarEnvironment {
    #[cfg(test)]
    fn from_command(command: Vec<String>, staging_root: PathBuf) -> Self {
        Self {
            command: Some(command),
            staging_root,
            terminate_notify: None,
            test_observer: None,
            uniform_probe: None,
        }
    }

    fn production() -> Self {
        let registry_path = bundle_registry_path();
        let staging_root = registry_path.parent().map_or_else(
            || PathBuf::from("activations"),
            |parent| parent.join("activations"),
        );
        Self {
            command: plugins::bundle_sidecar_command(),
            staging_root,
            #[cfg(test)]
            terminate_notify: None,
            #[cfg(test)]
            test_observer: None,
            #[cfg(test)]
            uniform_probe: None,
        }
    }
}

impl SidecarEnvironment for BundleSidecarEnvironment {
    fn factory_for(
        &self,
        binding: &TurnBinding,
        stable_agent_id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        #[cfg(test)]
        if let Some(observer) = &self.test_observer {
            observer.mark_resolution_hook(stable_agent_id);
        }
        // The agent must exist, but it need not own a bundle: a built-in owns no
        // bundle resources and therefore never has a sidecar.
        let definition = binding.resolve_agent(stable_agent_id).ok_or_else(|| {
            CoreError::AgentDefinitionMissing {
                agent_id: stable_agent_id.to_string(),
            }
        })?;
        if definition.origin.is_builtin() {
            return Ok(None);
        }
        let has_selected_sidecar_capability =
            binding.has_selected_bundle_sidecar_capability(stable_agent_id)?;
        if !has_selected_sidecar_capability {
            return Ok(None);
        }
        let Some(command) = &self.command else {
            return Err(CoreError::Invalid(
                "Bun is required for executable Bundle sidecars".to_string(),
            ));
        };
        Ok(Some(Arc::new(BundleSidecarFactory {
            binding: binding.clone(),
            stable_agent_id: stable_agent_id.to_string(),
            command: command.clone(),
            staging_root: self.staging_root.clone(),
            #[cfg(test)]
            terminate_notify: self.terminate_notify.clone(),
        })))
    }
}

struct BundleSidecarFactory {
    binding: TurnBinding,
    stable_agent_id: String,
    command: Vec<String>,
    staging_root: PathBuf,
    #[cfg(test)]
    terminate_notify: Option<Arc<tokio::sync::Notify>>,
}

struct BundleActivationDirGuard {
    path: PathBuf,
    armed: bool,
}

impl BundleActivationDirGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(mut self) -> PathBuf {
        self.armed = false;
        std::mem::take(&mut self.path)
    }
}

impl Drop for BundleActivationDirGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[async_trait::async_trait]
impl BoundSidecarFactory for BundleSidecarFactory {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        validate_activation_id(&start.activation_id)?;
        if self.command.is_empty() {
            return Err(CoreError::Invalid(
                "bundle sidecar command is empty".to_string(),
            ));
        }
        std::fs::create_dir_all(&self.staging_root).map_err(|error| {
            CoreError::Invalid(format!(
                "create bundle sidecar staging root `{}`: {error}",
                self.staging_root.display()
            ))
        })?;
        let activation_dir = self.staging_root.join(&start.activation_id);
        std::fs::create_dir(&activation_dir).map_err(|error| {
            CoreError::Invalid(format!(
                "create bundle sidecar activation directory `{}`: {error}",
                activation_dir.display()
            ))
        })?;
        let activation_dir = BundleActivationDirGuard::new(activation_dir);

        let extension_paths = materialize_bundle_sidecar_resources(
            &self.binding,
            &self.stable_agent_id,
            activation_dir.path(),
        )?;

        let mut command = self.command.clone();
        if !extension_paths.is_empty() {
            command.push("--".to_string());
            for path in extension_paths {
                if !path.is_absolute() {
                    return Err(CoreError::Invalid(format!(
                        "bundle extension path must be absolute: `{}`",
                        path.display()
                    )));
                }
                let Some(path) = path.to_str() else {
                    return Err(CoreError::Invalid(
                        "bundle extension path is not valid UTF-8".to_string(),
                    ));
                };
                command.push("--bundle-extension".to_string());
                command.push(path.to_string());
            }
        }

        let (client, mut guard) = match PluginClient::spawn_bundle(&command, activation_dir.path())
        {
            Ok(result) => result,
            Err(error) => return Err(CoreError::Invalid(format!("spawn bundle sidecar: {error}"))),
        };
        let lifecycle = match start.lifecycle {
            SidecarLifecycle::Transient => ActivationLifecycle::Transient,
            SidecarLifecycle::Resident => ActivationLifecycle::Resident,
        };
        let initialized = match client
            .initialize_activation(
                host_info(),
                ActivationMetadata {
                    activation_id: start.activation_id.clone(),
                    lifecycle,
                },
            )
            .await
        {
            Ok(initialized) => initialized,
            Err(error) => {
                let _ = guard.terminate().await;
                return Err(CoreError::Invalid(format!(
                    "initialize bundle sidecar: {error}"
                )));
            }
        };
        if initialized.protocol_version != hya_plugin::messages::PROTOCOL_VERSION
            || initialized.plugin.kind != PluginKindWire::Compat
        {
            let _ = guard.terminate().await;
            return Err(CoreError::Invalid(
                "bundle sidecar initialize declaration is incompatible".to_string(),
            ));
        }
        let tools_and_hooks = (|| {
            validate_bundle_sidecar_hooks(
                &self.binding,
                &self.stable_agent_id,
                &initialized.hooks,
            )?;
            let tools = bind_bundle_sidecar_tools(
                &self.binding,
                &self.stable_agent_id,
                &client,
                &initialized.tools,
            )?;
            let hooks = (!initialized.hooks.is_empty()).then(|| {
                Arc::new(hya_plugin::ActivationHookDispatcher::new(
                    client.clone(),
                    &initialized.hooks,
                )) as Arc<dyn hya_core::hooks::HookDispatcher>
            });
            Ok::<_, CoreError>((tools, hooks))
        })();
        let (tools, hooks) = match tools_and_hooks {
            Ok(bound) => bound,
            Err(error) => {
                let _ = guard.terminate().await;
                return Err(error);
            }
        };

        Ok(Box::new(BundleSidecarHandle {
            client,
            guard: Some(guard),
            activation_dir: Some(activation_dir.disarm()),
            tools,
            hooks,
            #[cfg(test)]
            terminate_notify: self.terminate_notify.clone(),
        }))
    }
}

struct BundleSidecarHandle {
    client: PluginClient,
    guard: Option<ChildGuard>,
    activation_dir: Option<PathBuf>,
    tools: Arc<[ResolvedTool]>,
    hooks: Option<Arc<dyn hya_core::hooks::HookDispatcher>>,
    #[cfg(test)]
    terminate_notify: Option<Arc<tokio::sync::Notify>>,
}

impl BundleSidecarHandle {
    fn cleanup_activation_dir(&mut self) -> Option<CoreError> {
        self.activation_dir.take().and_then(|activation_dir| {
            std::fs::remove_dir_all(&activation_dir).err().map(|error| {
                CoreError::Invalid(format!(
                    "remove bundle sidecar activation directory `{}`: {error}",
                    activation_dir.display()
                ))
            })
        })
    }
}

impl Drop for BundleSidecarHandle {
    fn drop(&mut self) {
        drop(self.guard.take());
        let _ = self.cleanup_activation_dir();
    }
}

#[async_trait::async_trait]
impl SidecarHandle for BundleSidecarHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<(), CoreError> {
        let mut shutdown_error = None;
        if let Some(mut guard) = self.guard.take()
            && let Err(error) = guard.shutdown().await
        {
            shutdown_error = Some(CoreError::Invalid(format!(
                "shutdown bundle sidecar: {error}"
            )));
        }

        let cleanup_error = self.cleanup_activation_dir();

        shutdown_error.or(cleanup_error).map_or(Ok(()), Err)
    }

    fn is_healthy(&self) -> bool {
        self.guard.is_some() && !self.client.is_closed()
    }

    fn loss_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        Some(self.client.closed_token())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        let mut terminate_error = None;
        if let Some(mut guard) = self.guard.take()
            && let Err(error) = guard.terminate().await
        {
            terminate_error = Some(CoreError::Invalid(format!(
                "terminate bundle sidecar: {error}"
            )));
        }

        let cleanup_error = self.cleanup_activation_dir();
        let result = terminate_error.or(cleanup_error).map_or(Ok(()), Err);
        #[cfg(test)]
        if let Some(notify) = &self.terminate_notify {
            notify.notify_one();
        }
        result
    }

    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        Arc::clone(&self.tools)
    }

    fn hook_dispatcher(&self) -> Option<Arc<dyn hya_core::hooks::HookDispatcher>> {
        self.hooks.clone()
    }
}

struct BundleSidecarTool {
    client: PluginClient,
    rpc_name: String,
    canonical_name: String,
    schema: ToolSchema,
}

#[async_trait::async_trait]
impl Tool for BundleSidecarTool {
    fn name(&self) -> &str {
        &self.canonical_name
    }

    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }

    async fn execute(
        &self,
        ctx: &ToolCtx,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let session = ctx.session.ok_or_else(|| {
            ToolError::Other("bundle sidecar tool requires a session".to_string())
        })?;
        ctx.permission
            .assert(Action::Tool, Resource::Tool(self.canonical_name.clone()))
            .await?;
        let result = tokio::select! {
            _ = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
            result = self.client.call_tool(
                &self.rpc_name,
                session,
                ctx.operation.source_tool_call_id(),
                input,
            ) => result,
        };
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                if self.client.is_closed() {
                    ctx.cancel.cancel();
                    return Err(ToolError::Cancelled);
                }
                return Err(ToolError::Other(format!(
                    "bundle sidecar tool call: {error}"
                )));
            }
        };
        if !reply.ok {
            return Err(ToolError::Other(reply.output.to_string()));
        }
        Ok(reply.output)
    }
}

fn bind_bundle_sidecar_tools(
    binding: &TurnBinding,
    stable_agent_id: &str,
    client: &PluginClient,
    declarations: &[ToolInfo],
) -> Result<Arc<[ResolvedTool]>, CoreError> {
    let (bundle_id, _) = binding
        .bundle_catalog()
        .resolve_agent_entry(stable_agent_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_agent_id.to_string(),
        })?;
    let policy = binding.agent_resource_policy(stable_agent_id)?;
    let expected = policy
        .selected_bundle_tool_ids()
        .iter()
        .map(|stable_id| {
            binding
                .bundle_catalog()
                .resolve_resource_entry(bundle_id, hya_bundle::ExportKind::Tool, stable_id)
                .map(|(_, resource)| resource)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut declaration_names = std::collections::BTreeSet::new();
    for declaration in declarations {
        if !declaration_names.insert(declaration.name.clone()) {
            return Err(CoreError::Invalid(format!(
                "duplicate bundle sidecar tool declaration `{}`",
                declaration.name
            )));
        }
        if declaration
            .input_schema
            .get("type")
            .and_then(|value| value.as_str())
            != Some("object")
        {
            return Err(CoreError::Invalid(format!(
                "bundle sidecar tool `{}` input schema must be an object",
                declaration.name
            )));
        }
    }
    if expected.len() != declarations.len() {
        return Err(CoreError::Invalid(format!(
            "bundle sidecar tool declaration count mismatch: expected {}, got {}",
            expected.len(),
            declarations.len()
        )));
    }

    let mut bound = Vec::with_capacity(expected.len());
    for resource in expected {
        let declaration = declarations
            .iter()
            .find(|declaration| declaration.name == resource.local_id)
            .ok_or_else(|| {
                CoreError::Invalid(format!(
                    "missing bundle sidecar tool declaration `{}`",
                    resource.local_id
                ))
            })?;
        let schema = ToolSchema {
            name: ToolName::new(resource.stable_id.clone()),
            description: declaration.description.clone(),
            input_schema: declaration.input_schema.clone(),
            output_schema: None,
        };
        bound.push(ResolvedTool {
            tool: Arc::new(BundleSidecarTool {
                client: client.clone(),
                rpc_name: declaration.name.clone(),
                canonical_name: resource.stable_id.clone(),
                schema,
            }),
            permission: ToolPermission::Tool,
        });
    }
    Ok(Arc::from(bound))
}

fn declared_bundle_sidecar_hooks(
    registrations: &[HookRegistration],
) -> Result<BTreeSet<HookName>, CoreError> {
    let mut seen = BTreeSet::new();
    for registration in registrations {
        if !matches!(
            registration.name,
            HookName::ToolExecuteBefore | HookName::ToolExecuteAfter | HookName::Event
        ) {
            return Err(CoreError::Invalid(format!(
                "unsupported Bundle sidecar hook declaration `{}`",
                registration.name.as_str()
            )));
        }
        if !seen.insert(registration.name) {
            return Err(CoreError::Invalid(format!(
                "duplicate Bundle sidecar hook declaration `{}`",
                registration.name.as_str()
            )));
        }
    }
    Ok(seen)
}

fn validate_bundle_sidecar_hooks(
    binding: &TurnBinding,
    stable_agent_id: &str,
    registrations: &[HookRegistration],
) -> Result<(), CoreError> {
    let actual = declared_bundle_sidecar_hooks(registrations)?;
    let (bundle_id, _) = binding
        .bundle_catalog()
        .resolve_agent_entry(stable_agent_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_agent_id.to_string(),
        })?;
    let policy = binding.agent_resource_policy(stable_agent_id)?;
    let mut expected = BTreeSet::new();
    for stable_id in policy.canonical_hook_ids() {
        let (_, resource) = binding.bundle_catalog().resolve_resource_entry(
            bundle_id,
            hya_bundle::ExportKind::Hook,
            stable_id,
        )?;
        let hook = match resource.local_id.as_str() {
            "event" => HookName::Event,
            "tool.execute.before" => HookName::ToolExecuteBefore,
            "tool.execute.after" => HookName::ToolExecuteAfter,
            _ => {
                return Err(CoreError::Invalid(format!(
                    "unsupported Bundle sidecar hook resource `{}`",
                    resource.local_id
                )));
            }
        };
        expected.insert(hook);
    }
    if actual != expected {
        return Err(CoreError::Invalid(format!(
            "Bundle sidecar hook declaration set mismatch: expected {expected:?}, got {actual:?}"
        )));
    }
    Ok(())
}

fn validate_activation_id(activation_id: &str) -> Result<(), CoreError> {
    let mut components = Path::new(activation_id).components();
    if activation_id.is_empty()
        || activation_id.contains(['/', '\\', ':'])
        || activation_id.as_bytes().contains(&0)
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(CoreError::Invalid(format!(
            "unsafe bundle sidecar activation id `{activation_id}`"
        )));
    }
    Ok(())
}

/// Agent base only — for HTTP/SSE server and interactive TUI AppState assembly.
///
/// Bundle `prompt=None` keeps this base; per-turn server discovery appends
/// Environment + current workdir AGENTS + references. Baking those at startup
/// would duplicate AGENTS when guidance is also layered.
pub fn agent_base_with_model(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new(model),
        system_prompt: HARNESS_AGENT_BASE.to_string(),
        workdir: PathBuf::from("."),
        reasoning,
    }
}

/// Direct-mode agent (exec/RPC/goal): base + Environment + process-cwd AGENTS.
///
/// These paths call `run_turn` without a separate guidance layer, so context
/// must remain composed into `system_prompt` here.
pub fn agent_with_model(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    let workdir = PathBuf::from(".");
    let env = PromptEnv {
        cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string()),
        platform: std::env::consts::OS.to_string(),
        date: today(),
    };
    let context = discover_context_files(&workdir);
    let system_prompt = build_system_prompt(HARNESS_AGENT_BASE, &env, &context);
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new(model),
        system_prompt,
        workdir,
        reasoning,
    }
}

/// First-run guidance produced when no usable config is found and hya falls
/// back to the offline echo provider.
///
/// It is carried as *data* on [`RuntimeConfig`] rather than printed at the point
/// of resolution: that keeps it out of machine-readable surfaces (JSONL RPC,
/// `exec`/`-p` piping, `serve`), which never call [`OfflineNotice::emit`]. Only
/// interactive startup paths surface it, and always to stderr — never stdout.
pub struct OfflineNotice {
    /// Where a config file is expected (and should be created).
    pub config_path: PathBuf,
}

impl OfflineNotice {
    /// Render the multi-line guidance: what happened, that hya is offline, and
    /// how to connect a real model.
    #[must_use]
    pub fn render(&self) -> String {
        let path = self.config_path.display();
        format!(
            "hya: no usable provider config found at {path}\n\
             hya: running in OFFLINE mode — the built-in provider only echoes input, \
             so models cannot reason or use tools.\n\
             hya: to connect a real model, edit {path} (see docs/configuration.md)\n\
             hya:   and/or save a provider token with `hya login <provider> <token>`."
        )
    }

    /// Print the notice to stderr so it never corrupts machine-readable stdout.
    pub fn emit(&self) {
        eprintln!("{}", self.render());
    }
}

/// Resolved provider/MCP/plugin/permission inputs ready to build a session engine.
pub struct RuntimeConfig {
    /// Live model routes (or offline dev provider).
    pub router: ProviderRouter,
    /// Active default model id for new sessions.
    pub model: String,
    /// Default reasoning effort for the active model, when configured.
    pub reasoning: Option<ReasoningEffort>,
    /// Catalog models from config (empty when offline).
    pub models: Vec<config::ModelEntry>,
    /// MCP server configs to connect at engine build.
    pub mcp: BTreeMap<String, McpServerConfig>,
    /// Plugin specs already merged from config + manifests.
    pub plugins: Vec<PluginSpec>,
    /// Preferred primary agent when workdir does not select one.
    pub default_agent: Option<String>,
    /// Logical model categories the runtime resolves at subagent spawn time.
    pub categories: CategoryRegistry,
    /// Set when no usable config was found and the offline provider was chosen.
    /// Interactive startup emits it; headless/machine-readable modes ignore it.
    pub offline_notice: Option<OfflineNotice>,
    /// Tool permission policy for the engine.
    pub permission: InvocationPolicy,
    /// Web-search plane configuration.
    pub websearch: WebSearchConfig,
}

impl RuntimeConfig {
    /// When `yolo` is true, force [`PermissionModel::Danger`] (auto-approve all tools).
    #[must_use]
    pub fn with_yolo(mut self, yolo: bool) -> Self {
        if yolo {
            self.permission = self.permission.with_model(PermissionModel::Danger);
        }
        self
    }
}

/// Resolve a provider router + active model from hya's config, falling back
/// to the offline echo provider when no usable config is present.
pub fn resolve_runtime(model_override: Option<String>) -> RuntimeConfig {
    match config::load() {
        Ok(Some(cfg)) => {
            let (fallback_router, fallback_model) = offline_router(model_override.clone());
            let default_model = if cfg.default_model.is_empty() {
                fallback_model
            } else {
                cfg.default_model
            };

            let model = model_override
                .or_else(|| std::env::var("HYA_MODEL").ok())
                .unwrap_or(default_model);
            let reasoning = cfg
                .models
                .iter()
                .find(|entry| entry.matches_model_ref(&model))
                .and_then(|entry| entry.reasoning_default);
            RuntimeConfig {
                router: if cfg.has_providers {
                    cfg.router
                } else {
                    fallback_router
                },
                model,
                reasoning,
                models: cfg.models,
                mcp: cfg.mcp,
                plugins: plugins::resolve(cfg.plugins, plugins::plugins_dir().as_deref()),
                default_agent: cfg.default_agent,
                categories: cfg.categories,
                offline_notice: None,
                permission: cfg.permission,
                websearch: cfg.websearch,
            }
        }
        Ok(None) => {
            let (router, model) = offline_router(model_override);
            RuntimeConfig {
                router,
                model,
                reasoning: None,
                models: Vec::new(),
                mcp: BTreeMap::new(),
                plugins: Vec::new(),
                default_agent: None,
                categories: CategoryRegistry::default(),
                offline_notice: Some(OfflineNotice {
                    config_path: config::expected_config_path(),
                }),
                permission: InvocationPolicy::default(),
                websearch: WebSearchConfig::default(),
            }
        }
        Err(e) => {
            eprintln!("hya: config error ({e:#}); using the offline provider");
            let (router, model) = offline_router(model_override);
            RuntimeConfig {
                router,
                model,
                reasoning: None,
                models: Vec::new(),
                mcp: BTreeMap::new(),
                plugins: Vec::new(),
                default_agent: None,
                categories: CategoryRegistry::default(),
                offline_notice: None,
                permission: InvocationPolicy::default().with_model(PermissionModel::Strict),
                websearch: WebSearchConfig::default(),
            }
        }
    }
}

/// Open a SQLite session store at `db`, or an in-memory store when `db` is empty.
pub async fn open_store(db: &str) -> anyhow::Result<SessionStore> {
    if db.is_empty() {
        SessionStore::connect_memory()
            .await
            .context("open in-memory store")
    } else {
        SessionStore::connect(db)
            .await
            .with_context(|| format!("open store at {db}"))
    }
}

#[derive(Serialize)]
struct SpawnRequestFingerprint<'a> {
    domain: &'static str,
    parent: SessionId,
    background: bool,
    members: &'a [SpawnMember],
}

fn spawn_request_fingerprint(req: &SpawnRequest) -> Result<[u8; 32], serde_json::Error> {
    let canonical = serde_json::to_vec(&SpawnRequestFingerprint {
        domain: "hya.spawn-admission.v1",
        parent: req.parent,
        background: req.background,
        members: &req.members,
    })?;
    Ok(Sha256::digest(canonical).into())
}

#[allow(dead_code)]
const ADMISSION_BINDING_DOMAIN_V1: &[u8] = b"hya.admission-binding.v1";
#[allow(dead_code)]
const RESOLVER_SEMANTICS_V1: &[u8] = b"ResolverSemanticsV1";
#[allow(dead_code)]
const EMPTY_CATEGORY_IDENTITY_V1: &[u8] = b"CategoryRegistryEmptyV1";
#[allow(dead_code)]
const EMPTY_PROVIDER_IDENTITY_V1: &[u8] = b"ProviderRouterEmptyV1";
#[allow(dead_code)]
const PROVIDER_RESOLUTION_IDENTITY_V1: &[u8] = b"ProviderResolutionV1";
#[allow(dead_code)]
const CATEGORY_RESOLUTION_IDENTITY_V1: &[u8] = b"CategoryResolutionV1";

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmissionResolutionContextError {
    ProviderIdentityUnavailable,
    CanonicalLengthOverflow,
}

/// Immutable app-owned inputs used to resolve and fingerprint one admission.
///
/// The runtime and store layers receive only the resulting opaque fingerprint;
/// these exact objects remain private to the app resolution seam.
#[allow(dead_code)]
struct AdmissionResolutionContext {
    base: AgentSpec,
    categories: Arc<CategoryRegistry>,
    router: Arc<ProviderRouter>,
    base_model_len: u64,
    base_system_prompt_len: u64,
    base_reasoning_len: u64,
    category_resolution: Vec<u8>,
    provider_resolution: Vec<u8>,
}

#[allow(dead_code)]
impl AdmissionResolutionContext {
    fn capture(
        base: AgentSpec,
        categories: Arc<CategoryRegistry>,
        router: Arc<ProviderRouter>,
    ) -> Result<Self, AdmissionResolutionContextError> {
        let base_model_len = canonical_length(base.model.as_str().as_bytes())?;
        let base_system_prompt_len = canonical_length(base.system_prompt.as_bytes())?;
        let base_reasoning_len = canonical_length(base_reasoning(&base).as_bytes())?;
        let category_resolution = canonical_category_resolution(&categories)?;
        let provider_resolution = canonical_provider_resolution(&router)?;

        Ok(Self {
            base,
            categories,
            router,
            base_model_len,
            base_system_prompt_len,
            base_reasoning_len,
            category_resolution,
            provider_resolution,
        })
    }

    fn admission_binding_fingerprint_v1(&self, runtime_fingerprint: [u8; 32]) -> [u8; 32] {
        let mut canonical = Vec::new();
        canonical.extend_from_slice(ADMISSION_BINDING_DOMAIN_V1);
        canonical.extend_from_slice(RESOLVER_SEMANTICS_V1);
        append_len_prefixed(&mut canonical, &runtime_fingerprint, 32);
        append_len_prefixed(
            &mut canonical,
            self.base.model.as_str().as_bytes(),
            self.base_model_len,
        );
        append_len_prefixed(
            &mut canonical,
            self.base.system_prompt.as_bytes(),
            self.base_system_prompt_len,
        );
        append_len_prefixed(
            &mut canonical,
            base_reasoning(&self.base).as_bytes(),
            self.base_reasoning_len,
        );
        canonical.extend_from_slice(&self.category_resolution);
        canonical.extend_from_slice(&self.provider_resolution);
        Sha256::digest(canonical).into()
    }

    fn resolve_agent_for_binding(
        &self,
        engine: &SessionEngine,
        binding: &TurnBinding,
        stable_id: &str,
    ) -> Result<AgentSpec, CoreError> {
        engine.agent_spec_for_binding(binding, &self.base, stable_id)
    }

    fn resolve_category_for_admission(&self, category: &str) -> Option<ModelRef> {
        self.categories
            .resolve_servable(category, |model| self.router.resolve(model).is_some())
            .map(|resolved| resolved.model)
    }
}

#[allow(dead_code)]
struct PreparedSpawnAdmission {
    request_fingerprint: [u8; 32],
    resolution: AdmissionResolutionContext,
    intents: Vec<AdmissionIntent>,
}

#[allow(dead_code)]
fn canonical_length(bytes: &[u8]) -> Result<u64, AdmissionResolutionContextError> {
    u64::try_from(bytes.len()).map_err(|_| AdmissionResolutionContextError::CanonicalLengthOverflow)
}

#[allow(dead_code)]
fn canonical_count(count: usize) -> Result<u64, AdmissionResolutionContextError> {
    u64::try_from(count).map_err(|_| AdmissionResolutionContextError::CanonicalLengthOverflow)
}

#[allow(dead_code)]
fn append_len_prefixed(canonical: &mut Vec<u8>, bytes: &[u8], length: u64) {
    canonical.extend_from_slice(&length.to_be_bytes());
    canonical.extend_from_slice(bytes);
}

#[allow(dead_code)]
fn canonical_category_resolution(
    categories: &CategoryRegistry,
) -> Result<Vec<u8>, AdmissionResolutionContextError> {
    let entries = categories.resolution_candidates();
    if entries.is_empty() {
        return Ok(EMPTY_CATEGORY_IDENTITY_V1.to_vec());
    }

    let mut canonical = Vec::new();
    canonical.extend_from_slice(CATEGORY_RESOLUTION_IDENTITY_V1);
    canonical.extend_from_slice(&canonical_count(entries.len())?.to_be_bytes());
    for (category, candidates) in entries {
        let category_bytes = category.as_bytes();
        append_len_prefixed(
            &mut canonical,
            category_bytes,
            canonical_length(category_bytes)?,
        );
        canonical.extend_from_slice(&canonical_count(candidates.len())?.to_be_bytes());
        for candidate in candidates {
            let candidate_bytes = candidate.as_str().as_bytes();
            append_len_prefixed(
                &mut canonical,
                candidate_bytes,
                canonical_length(candidate_bytes)?,
            );
        }
    }
    Ok(canonical)
}

#[allow(dead_code)]
fn canonical_provider_resolution(
    router: &ProviderRouter,
) -> Result<Vec<u8>, AdmissionResolutionContextError> {
    let identities = router
        .configured_identities_v1()
        .ok_or(AdmissionResolutionContextError::ProviderIdentityUnavailable)?;
    if identities.is_empty() {
        return Ok(EMPTY_PROVIDER_IDENTITY_V1.to_vec());
    }

    let mut canonical = Vec::new();
    canonical.extend_from_slice(PROVIDER_RESOLUTION_IDENTITY_V1);
    canonical.extend_from_slice(&canonical_count(identities.len())?.to_be_bytes());
    for identity in identities {
        let identity_len = canonical_length(&identity)?;
        append_len_prefixed(&mut canonical, &identity, identity_len);
    }
    Ok(canonical)
}

#[allow(dead_code)]
fn base_reasoning(base: &AgentSpec) -> &'static str {
    base.reasoning
        .map(ReasoningEffort::as_str)
        .unwrap_or("none")
}

struct ResolvedSpawnMember {
    request: SpawnMember,
    authorized_target: AgentName,
    agent: AgentSpec,
    binding: TurnBinding,
    agents: Arc<[hya_tool::AgentDef]>,
    resources: AgentResourcePolicy,
    resident: bool,
    sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
    /// Immutable guidance Arc cloned from the spawn request (not from disk).
    guidance: Option<Arc<str>>,
}

/// Pre-admission team-root context for main-as-actor synthesis on resident batches.
///
/// Bound from the spawn TurnBinding + root projection stable AgentName — never from
/// a nested caller's roster/resource policy.
struct MainActivationContext {
    root: SessionId,
    agent: AgentSpec,
    binding: TurnBinding,
    agents: Arc<[hya_tool::AgentDef]>,
    resources: AgentResourcePolicy,
    guidance: Option<Arc<str>>,
}

/// Resolve exact team root + root definition/roster/resource policy before durable
/// spawn admission. Fail-closed; no parent-fallback and no catalog/base synthesis.
async fn resolve_main_activation_context(
    engine: &SessionEngine,
    binding: &TurnBinding,
    base: &AgentSpec,
    parent: SessionId,
    guidance: Option<Arc<str>>,
) -> Result<MainActivationContext, SpawnError> {
    let (root, _) = engine
        .session_lineage(parent)
        .await
        .map_err(|_| SpawnError::Unavailable)?;
    let root_projection = engine
        .read_projection(root)
        .await
        .map_err(|_| SpawnError::Unavailable)?;
    let root_agent_name = root_projection
        .session
        .agent
        .as_ref()
        .ok_or(SpawnError::Unavailable)?;
    let root_stable = root_agent_name.as_str();
    // Exact catalog lookup for the root stable id; map missing definition through
    // the existing typed UnknownAgentId seam (no general/base fallback).
    if binding.resolve_agent(root_stable).is_none() {
        return Err(SpawnError::UnknownAgentId {
            agent_id: root_stable.to_string(),
        });
    }
    let agent = engine
        .agent_spec_for_binding(binding, base, root_stable)
        .map_err(|_| SpawnError::Unavailable)?;
    let agents = engine
        .agent_roster_for_binding(binding, root_stable)
        .map_err(|err| match err {
            CoreError::Bundle(hya_bundle::BundleError::UnknownAgentId { agent_id })
            | CoreError::AgentDefinitionMissing { agent_id } => {
                SpawnError::UnknownAgentId { agent_id }
            }
            _ => SpawnError::Unavailable,
        })?;
    let resources = engine
        .agent_resource_policy_for_binding(binding, root_stable)
        .map_err(|err| match err {
            CoreError::Bundle(hya_bundle::BundleError::UnknownAgentId { agent_id })
            | CoreError::AgentDefinitionMissing { agent_id } => {
                SpawnError::UnknownAgentId { agent_id }
            }
            _ => SpawnError::Unavailable,
        })?;
    Ok(MainActivationContext {
        root,
        agent,
        binding: binding.clone(),
        agents,
        resources,
        guidance,
    })
}

/// Batch-scoped shared inputs for [`resolve_spawn_member`].
///
/// One captured [`TurnBinding`] and request guidance Arc per batch; [`SpawnMember`]
/// remains the only per-member argument.
struct ResolveSpawnMemberCtx<'a> {
    engine: &'a SessionEngine,
    binding: &'a TurnBinding,
    base: &'a AgentSpec,
    caller: &'a str,
    allowed_agents: &'a [hya_tool::AgentDef],
    categories: &'a CategoryRegistry,
    is_servable: &'a dyn Fn(&ModelRef) -> bool,
    guidance: Option<Arc<str>>,
    sidecar_environment: &'a dyn SidecarEnvironment,
}

#[allow(dead_code)]
struct AdmissionLaunchResolutionCtx<'a> {
    engine: &'a SessionEngine,
    binding: &'a TurnBinding,
    resolution: &'a AdmissionResolutionContext,
    caller: &'a str,
    allowed_agents: &'a [hya_tool::AgentDef],
    guidance: Option<Arc<str>>,
    sidecar_environment: &'a dyn SidecarEnvironment,
}

#[allow(dead_code)]
struct ResolvedAdmissionLaunch {
    launch: AdmissionLaunch,
    intent: SpawnIntentV1,
    resolved: ResolvedSpawnMember,
}

#[allow(dead_code)]
struct InstalledAdmissionTask<T> {
    operation_id: hya_proto::OperationId,
    member_ordinal: u32,
    start_signal: Option<tokio::sync::oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<T>>,
}

#[allow(dead_code)]
struct TransientAdmissionCompletion {
    operation_id: hya_proto::OperationId,
    member_ordinal: u32,
    evidence: MemberEvidence,
    promoted: Vec<AdmissionLaunch>,
}

type TransientAdmissionResult = Result<TransientAdmissionCompletion, SpawnError>;
type TransientAdmissionHandle = tokio::task::JoinHandle<Result<(), SpawnError>>;
type TransientAdmissionCompletionSender = tokio::sync::mpsc::Sender<TransientAdmissionResult>;
type TransientAdmissionCompletionReceiver = tokio::sync::mpsc::Receiver<TransientAdmissionResult>;

#[allow(dead_code)]
fn install_admission_task<F, T>(
    launch: &AdmissionLaunch,
    work_future: F,
) -> InstalledAdmissionTask<T>
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (start_signal, signal) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        match signal.await {
            Ok(()) => work_future.await,
            Err(_) => std::future::pending::<T>().await,
        }
    });
    InstalledAdmissionTask {
        operation_id: launch.record.operation_id,
        member_ordinal: launch.record.member_ordinal,
        start_signal: Some(start_signal),
        handle: Some(handle),
    }
}

#[allow(dead_code)]
fn transient_admission_work_future(
    engine: Arc<SessionEngine>,
    resolved: ResolvedAdmissionLaunch,
    cancel: tokio_util::sync::CancellationToken,
    actor_claim: Option<hya_store::ActorClaim>,
) -> impl std::future::Future<Output = TransientAdmissionResult> + Send + 'static {
    let ResolvedAdmissionLaunch {
        launch,
        intent,
        resolved:
            ResolvedSpawnMember {
                request: member,
                authorized_target,
                agent,
                binding,
                agents,
                resources,
                guidance,
                sidecar_factory,
                ..
            },
    } = resolved;
    let _authorized_target = authorized_target;
    let operation_id = launch.record.operation_id;
    let member_ordinal = launch.record.member_ordinal;
    let parent = intent.parent();
    let source_tool_call = intent.source_tool_call_id();
    let spec = MemberSpec {
        id: MemberId::new(),
        agent,
        binding,
        agents,
        resources: Some(resources),
        guidance,
        directive: member.prompt,
        description: member.description,
        session: member
            .task_id
            .as_deref()
            .and_then(|task_id| task_id.parse::<SessionId>().ok()),
        sidecar_factory,
        tool_call: Some(source_tool_call),
    };
    async move {
        let evidence = match actor_claim {
            Some(claim) => {
                run_pre_admitted_team_for_actor(
                    engine.clone(),
                    parent,
                    vec![spec],
                    cancel.clone(),
                    claim,
                )
                .await
            }
            None => {
                run_pre_admitted_member(
                    engine.clone(),
                    parent,
                    spec,
                    cancel.clone(),
                    AdmissionMemberIdentity {
                        operation_id,
                        member_ordinal,
                    },
                )
                .await
            }
        };
        let failed =
            !matches!(evidence.as_slice(), [entry] if entry.status != MemberStatus::Failed);
        let (terminal, reason) = if cancel.is_cancelled() {
            (AdmissionTerminal::Cancelled, "spawn member cancelled")
        } else if failed {
            (AdmissionTerminal::Aborted, "spawn member failed")
        } else {
            (AdmissionTerminal::Completed, "spawn member completed")
        };
        let outcome = engine
            .store()
            .finalize_admission_members(
                &[(operation_id, member_ordinal)],
                terminal,
                reason,
                actor_claim.as_ref(),
            )
            .await
            .map_err(|_| SpawnError::Unavailable)?;
        let [evidence] = evidence.as_slice() else {
            return Err(SpawnError::Unavailable);
        };
        Ok(TransientAdmissionCompletion {
            operation_id,
            member_ordinal,
            evidence: evidence.clone(),
            promoted: outcome.promoted,
        })
    }
}

#[allow(dead_code)]
fn install_transient_admission_launch(
    engine: Arc<SessionEngine>,
    resolved: ResolvedAdmissionLaunch,
    cancel: tokio_util::sync::CancellationToken,
    actor_claim: Option<hya_store::ActorClaim>,
) -> InstalledAdmissionTask<TransientAdmissionResult> {
    let launch = resolved.launch.clone();
    install_admission_task(
        &launch,
        transient_admission_work_future(engine, resolved, cancel, actor_claim),
    )
}

fn install_transient_admission_launch_with_completion(
    engine: Arc<SessionEngine>,
    resolved: ResolvedAdmissionLaunch,
    cancel: tokio_util::sync::CancellationToken,
    actor_claim: Option<hya_store::ActorClaim>,
    completion: TransientAdmissionCompletionSender,
) -> InstalledAdmissionTask<Result<(), SpawnError>> {
    let launch = resolved.launch.clone();
    let work = transient_admission_work_future(engine, resolved, cancel, actor_claim);
    let work = async move {
        completion
            .send(work.await)
            .await
            .map_err(|_| SpawnError::Unavailable)
    };
    install_admission_task(&launch, work)
}

#[allow(dead_code)]
impl<T> InstalledAdmissionTask<T> {
    async fn cancel(&mut self) {
        self.start_signal.take();
        if let Some(handle) = self.handle.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    /// Arm an already-Started member's work future without a second CAS.
    fn start_signal_only(mut self) -> Result<tokio::task::JoinHandle<T>, SpawnError> {
        let Some(start_signal) = self.start_signal.take() else {
            return Err(SpawnError::Unavailable);
        };
        let Some(handle) = self.handle.take() else {
            return Err(SpawnError::Unavailable);
        };
        if start_signal.send(()).is_err() {
            handle.abort();
            return Err(SpawnError::Unavailable);
        }
        Ok(handle)
    }

    async fn start(
        mut self,
        store: &SessionStore,
        actor_claim: Option<&hya_store::ActorClaim>,
    ) -> Result<tokio::task::JoinHandle<T>, SpawnError> {
        let started = match store
            .start_admission_member(self.operation_id, self.member_ordinal, actor_claim)
            .await
        {
            Ok(hya_store::AdmissionStartOutcome::Started(record)) => record,
            Ok(hya_store::AdmissionStartOutcome::Existing(_)) | Err(_) => {
                self.cancel().await;
                return Err(SpawnError::Unavailable);
            }
        };
        if started.operation_id != self.operation_id
            || started.member_ordinal != self.member_ordinal
            || started.state != hya_store::AdmissionState::Started
        {
            self.cancel().await;
            return Err(SpawnError::Unavailable);
        }

        let Some(start_signal) = self.start_signal.take() else {
            self.cancel().await;
            return Err(SpawnError::Unavailable);
        };
        if start_signal.send(()).is_err() {
            self.cancel().await;
            return Err(SpawnError::Unavailable);
        }

        let Some(handle) = self.handle.take() else {
            self.cancel().await;
            return Err(SpawnError::Unavailable);
        };
        Ok(handle)
    }
}

impl<T> Drop for InstalledAdmissionTask<T> {
    fn drop(&mut self) {
        self.start_signal.take();
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

fn authorize_spawn_target<'a>(
    binding: &'a TurnBinding,
    allowed_agents: &[hya_tool::AgentDef],
    caller: &str,
    member: &SpawnMember,
) -> Result<AgentDefinition<'a>, SpawnError> {
    let requested = member.subagent_type.trim();
    let requested = if requested.is_empty() {
        "general"
    } else {
        requested
    };
    let definition =
        binding
            .resolve_agent(requested)
            .ok_or_else(|| SpawnError::UnknownAgentId {
                agent_id: requested.to_string(),
            })?;
    if !allowed_agents
        .iter()
        .any(|allowed| allowed.name == definition.stable_id)
    {
        return Err(SpawnError::AgentSpawnNotAllowed {
            caller: caller.to_string(),
            agent_id: definition.stable_id.to_string(),
        });
    }
    Ok(definition)
}

fn validate_unsupported_inline_agent_fields(member: &SpawnMember) -> Result<(), SpawnError> {
    if member
        .inline_agent
        .as_ref()
        .is_some_and(|inline| inline.description.is_some())
    {
        return Err(SpawnError::UnsupportedInlineAgentField {
            field: "description",
        });
    }
    Ok(())
}

/// Durable journal admission owner path without fully resolving members.
///
/// Authorization and the prepared bundle lifecycle are the only semantic inputs
/// here; raw request overlays can opt a member into the resident lifecycle. An
/// authorization failure falls through to the legacy resolver so its existing
/// typed error remains public behavior.
///
/// Eligible:
/// - foreground multi/single-member all-transient batches (whole-batch reply)
/// - single-member background all-transient (running reply after registration)
///
/// Multi-member background and any resident batch stay on the legacy route.
fn uses_durable_admission_owner(binding: &TurnBinding, caller: &str, req: &SpawnRequest) -> bool {
    if req.members.is_empty() {
        return false;
    }
    let all_transient = req.members.iter().all(|member| {
        let Ok(definition) = authorize_spawn_target(binding, &req.agents, caller, member) else {
            return false;
        };
        definition.spawn_lifecycle != SpawnLifecycle::Resident
            && !member.resident
            && !member
                .inline_agent
                .as_ref()
                .and_then(|inline| inline.resident)
                .unwrap_or(false)
    });
    if !all_transient {
        return false;
    }
    if req.background {
        req.members.len() == 1
    } else {
        true
    }
}

/// How the durable admission owner answers the caller oneshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DurableOwnerReplyMode {
    /// Wait until every ordinal is terminal; reply `Ok(done outcomes)`.
    ForegroundWholeBatch,
    /// Reply `Ok(running)` once after Started + real session registration; detach.
    BackgroundRunningOnRegister,
}

#[allow(dead_code)]
fn prepare_spawn_admission(
    engine: &SessionEngine,
    binding: &TurnBinding,
    base: AgentSpec,
    categories: Arc<CategoryRegistry>,
    router: Arc<ProviderRouter>,
    caller: &str,
    req: &SpawnRequest,
) -> Result<PreparedSpawnAdmission, SpawnError> {
    req.members
        .iter()
        .try_for_each(validate_unsupported_inline_agent_fields)?;
    let request_fingerprint =
        spawn_request_fingerprint(req).map_err(|_| SpawnError::Unavailable)?;
    let runtime_fingerprint = engine
        .runtime_semantic_fingerprint_v1(binding)
        .ok_or(SpawnError::Unavailable)?;
    let resolution = AdmissionResolutionContext::capture(base, categories, router)
        .map_err(|_| SpawnError::Unavailable)?;
    let admission_binding_fingerprint =
        resolution.admission_binding_fingerprint_v1(runtime_fingerprint);
    let batch_cardinality = u32::try_from(req.members.len()).map_err(|_| SpawnError::Overloaded)?;
    let diagnostic_generation = binding.generation().get();
    let intents = req
        .members
        .iter()
        .cloned()
        .enumerate()
        .map(|(ordinal, member)| {
            let definition = authorize_spawn_target(binding, req.agents.as_ref(), caller, &member)?;
            let member_ordinal = u32::try_from(ordinal).map_err(|_| SpawnError::Overloaded)?;
            SpawnIntentV1::new(SpawnIntentInputV1 {
                member,
                parent: req.parent,
                stable_target: hya_proto::AgentName::new(definition.stable_id),
                background: req.background,
                operation: req.operation,
                member_ordinal,
                batch_cardinality,
                prior_start: PriorStartV1::NeverStarted,
                runtime_fingerprint,
                admission_binding_fingerprint,
                diagnostic_generation,
            })
            .map_err(|_| SpawnError::Unavailable)?
            .into_admission_intent()
            .map_err(|_| SpawnError::Unavailable)
        })
        .collect::<Result<Vec<_>, SpawnError>>()?;

    Ok(PreparedSpawnAdmission {
        request_fingerprint,
        resolution,
        intents,
    })
}

fn resolve_spawn_member(
    ctx: &ResolveSpawnMemberCtx<'_>,
    member: SpawnMember,
) -> Result<ResolvedSpawnMember, SpawnError> {
    let definition = authorize_spawn_target(ctx.binding, ctx.allowed_agents, ctx.caller, &member)?;
    resolve_authorized_spawn_member(ctx, member, &definition)
}

fn resolve_authorized_spawn_member(
    ctx: &ResolveSpawnMemberCtx<'_>,
    member: SpawnMember,
    definition: &AgentDefinition<'_>,
) -> Result<ResolvedSpawnMember, SpawnError> {
    validate_unsupported_inline_agent_fields(&member)?;
    let authorized_target = hya_proto::AgentName::new(definition.stable_id);
    let sidecar_factory = ctx
        .sidecar_environment
        .factory_for(ctx.binding, authorized_target.as_str())
        .map_err(|_| SpawnError::Unavailable)?;
    let mut agent = ctx
        .engine
        .agent_spec_for_binding(ctx.binding, ctx.base, definition.stable_id)
        .map_err(|_| SpawnError::Unavailable)?;
    let agents = ctx
        .engine
        .agent_roster_for_binding(ctx.binding, definition.stable_id)
        .map_err(|_| SpawnError::Unavailable)?;
    let resources = ctx
        .engine
        .agent_resource_policy_for_binding(ctx.binding, definition.stable_id)
        .map_err(|_| SpawnError::Unavailable)?;

    let resolve_category = |name: &str| {
        ctx.categories
            .resolve_servable(name, ctx.is_servable)
            .map(|resolved| resolved.model)
    };
    if let Some(model) = definition
        .model_policy
        .category
        .as_deref()
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    if let Some(model) = member
        .inline_agent
        .as_ref()
        .and_then(|inline| inline.category.as_deref())
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    if let Some(model) = definition.model_policy.model.as_deref() {
        agent.model = ModelRef::new(model);
    }
    if let Some(model) = member
        .inline_agent
        .as_ref()
        .and_then(|inline| inline.model.as_deref())
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        agent.model = ModelRef::new(model);
    }
    if let Some(model) = member
        .category
        .as_deref()
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    if let Some(model) = member
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        agent.model = ModelRef::new(model);
    }

    let mut resident = definition.spawn_lifecycle == SpawnLifecycle::Resident || member.resident;
    if let Some(inline) = member.inline_agent.as_ref() {
        resident |= inline.resident.unwrap_or(false);
        if !inline.prompt.trim().is_empty() {
            agent.system_prompt = inline.prompt.clone();
        }
        if !inline.name.trim().is_empty() {
            agent.name = AgentName::new(&inline.name);
        }
    }

    Ok(ResolvedSpawnMember {
        request: member,
        authorized_target,
        agent,
        binding: ctx.binding.clone(),
        agents,
        resources,
        resident,
        sidecar_factory,
        guidance: ctx.guidance.clone(),
    })
}

#[allow(dead_code)]
fn resolve_admission_launches(
    ctx: &AdmissionLaunchResolutionCtx<'_>,
    launches: Vec<AdmissionLaunch>,
) -> Result<Vec<ResolvedAdmissionLaunch>, SpawnError> {
    let runtime_fingerprint = ctx
        .engine
        .runtime_semantic_fingerprint_v1(ctx.binding)
        .ok_or(SpawnError::Unavailable)?;
    let admission_binding_fingerprint = ctx
        .resolution
        .admission_binding_fingerprint_v1(runtime_fingerprint);
    let is_servable = |model: &ModelRef| ctx.resolution.router.resolve(model).is_some();
    let resolve_ctx = ResolveSpawnMemberCtx {
        engine: ctx.engine,
        binding: ctx.binding,
        base: &ctx.resolution.base,
        caller: ctx.caller,
        allowed_agents: ctx.allowed_agents,
        categories: &ctx.resolution.categories,
        is_servable: &is_servable,
        guidance: ctx.guidance.clone(),
        sidecar_environment: ctx.sidecar_environment,
    };

    let decoded = launches
        .into_iter()
        .map(|launch| {
            let intent = SpawnIntentV1::decode_admission_launch(&launch)
                .map_err(|_| SpawnError::Unavailable)?;
            if launch.intent.runtime_fingerprint != runtime_fingerprint
                || launch.intent.admission_binding_fingerprint != admission_binding_fingerprint
            {
                return Err(SpawnError::Unavailable);
            }
            Ok((launch, intent))
        })
        .collect::<Result<Vec<_>, SpawnError>>()?;

    decoded
        .into_iter()
        .map(|(launch, intent)| {
            let definition = authorize_spawn_target(
                ctx.binding,
                ctx.allowed_agents,
                ctx.caller,
                intent.raw_member(),
            )?;
            if definition.stable_id != intent.stable_target().as_str() {
                return Err(SpawnError::Unavailable);
            }
            let resolved = resolve_authorized_spawn_member(
                &resolve_ctx,
                intent.raw_member().clone(),
                &definition,
            )?;
            Ok(ResolvedAdmissionLaunch {
                launch,
                intent,
                resolved,
            })
        })
        .collect()
}

#[allow(dead_code)]
async fn resolve_current_admission_launches(
    ctx: &AdmissionLaunchResolutionCtx<'_>,
    launch: AdmissionLaunch,
    wake_router: Option<&ForegroundAdmissionWakeRouter>,
) -> Result<Vec<ResolvedAdmissionLaunch>, SpawnError> {
    resolve_admission_launches_fifo(ctx.engine, launch, wake_router, |launch| {
        std::future::ready(resolve_admission_launches(ctx, vec![launch]))
    })
    .await
}

async fn resolve_admission_launches_fifo<F, Fut>(
    engine: &SessionEngine,
    launch: AdmissionLaunch,
    wake_router: Option<&ForegroundAdmissionWakeRouter>,
    mut resolve_one: F,
) -> Result<Vec<ResolvedAdmissionLaunch>, SpawnError>
where
    F: FnMut(AdmissionLaunch) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<ResolvedAdmissionLaunch>, SpawnError>>,
{
    let owner_operation_id = launch.record.operation_id;
    let mut pending = std::collections::VecDeque::from([launch]);
    let mut resolved = Vec::new();
    while let Some(launch) = pending.pop_front() {
        let records = engine
            .store()
            .admissions(launch.record.operation_id)
            .await
            .map_err(|_| SpawnError::Unavailable)?;
        if !records.iter().any(|record| {
            record == &launch.record && record.state == hya_store::AdmissionState::Accepted
        }) {
            continue;
        }

        match resolve_one(launch.clone()).await {
            Ok(mut launches) => resolved.append(&mut launches),
            Err(_) => {
                let outcome = engine
                    .store()
                    .finalize_admission_members(
                        &[(launch.record.operation_id, launch.record.member_ordinal)],
                        AdmissionTerminal::Aborted,
                        "admission recovery resolution unavailable",
                        None,
                    )
                    .await
                    .map_err(|_| SpawnError::Unavailable)?;
                for promoted in outcome.promoted {
                    if wake_router.is_none() || promoted.record.operation_id == owner_operation_id {
                        pending.push_back(promoted);
                    } else if let Some(wake_router) = wake_router {
                        wake_router.wake(promoted.record.operation_id);
                    }
                }
            }
        }
    }
    Ok(resolved)
}

#[allow(dead_code)]
async fn resolve_recovered_admission_launches(
    engine: &SessionEngine,
    resolution: &AdmissionResolutionContext,
    sidecar_environment: &dyn SidecarEnvironment,
    launch: AdmissionLaunch,
) -> Result<Vec<ResolvedAdmissionLaunch>, SpawnError> {
    resolve_admission_launches_fifo(engine, launch, None, |launch| async move {
        let intent =
            SpawnIntentV1::decode_admission_launch(&launch).map_err(|_| SpawnError::Unavailable)?;
        let projection = engine
            .read_projection(intent.parent())
            .await
            .map_err(|_| SpawnError::Unavailable)?;
        let caller = projection.session.agent.ok_or(SpawnError::Unavailable)?;
        let workdir = projection
            .session
            .workdir
            .map(PathBuf::from)
            .unwrap_or_else(|| resolution.base.workdir.clone());
        let binding = engine
            .bind_root_runtime(&workdir)
            .await
            .map_err(|_| SpawnError::Unavailable)?;
        let allowed_agents = engine
            .agent_roster_for_binding(&binding, caller.as_str())
            .map_err(|_| SpawnError::Unavailable)?;
        let ctx = AdmissionLaunchResolutionCtx {
            engine,
            binding: &binding,
            resolution,
            caller: caller.as_str(),
            allowed_agents: allowed_agents.as_ref(),
            guidance: None,
            sidecar_environment,
        };
        let resolved =
            resolve_admission_launches(&ctx, vec![launch]).map_err(|_| SpawnError::Unavailable)?;
        if resolved.len() != 1 {
            return Err(SpawnError::Unavailable);
        }
        Ok(resolved)
    })
    .await
}

fn admission_records_match(
    records: &[hya_store::AdmissionRecord],
    operation_id: hya_proto::OperationId,
    actor_claim: Option<&hya_store::ActorClaim>,
    cardinality: u32,
    terminal: bool,
) -> bool {
    let expected_actor = actor_claim.map(|claim| hya_store::AdmissionActorBinding {
        actor_id: claim.actor_id,
        actor_epoch: claim.epoch,
    });
    let Some(cardinality_usize) = usize::try_from(cardinality).ok() else {
        return false;
    };
    records.len() == cardinality_usize
        && records.iter().enumerate().all(|(index, record)| {
            record.operation_id == operation_id
                && record.member_ordinal == u32::try_from(index).unwrap_or(u32::MAX)
                && record.batch_size == cardinality
                && record.admission_units == 1
                && record.actor == expected_actor
                && (!terminal || record.state.is_terminal())
        })
}

#[allow(clippy::too_many_arguments)]
async fn cleanup_transient_admission<T>(
    engine: &SessionEngine,
    operation_id: hya_proto::OperationId,
    actor_claim: Option<&hya_store::ActorClaim>,
    cancel: &tokio_util::sync::CancellationToken,
    handles: &mut Vec<tokio::task::JoinHandle<T>>,
    cardinality: u32,
    reason: &str,
    wake_router: Option<&ForegroundAdmissionWakeRouter>,
) -> bool
where
    T: Send + 'static,
{
    cancel.cancel();
    for handle in handles.iter() {
        handle.abort();
    }
    for handle in handles.drain(..) {
        let _ = handle.await;
    }

    let records = match engine.store().admissions(operation_id).await {
        Ok(records)
            if admission_records_match(&records, operation_id, actor_claim, cardinality, false) =>
        {
            records
        }
        _ => return false,
    };
    let pending: Vec<_> = records
        .iter()
        .filter(|record| !record.state.is_terminal())
        .map(|record| (record.operation_id, record.member_ordinal))
        .collect();
    if !pending.is_empty() {
        let outcome = match engine
            .store()
            .finalize_admission_members(&pending, AdmissionTerminal::Aborted, reason, actor_claim)
            .await
        {
            Ok(outcome) => outcome,
            Err(_) => return false,
        };
        // Wake foreign owners only after the cleanup finalize commits.
        if let Some(wake_router) = wake_router {
            for promoted in outcome.promoted {
                if promoted.record.operation_id != operation_id {
                    wake_router.wake(promoted.record.operation_id);
                }
            }
        }
    }
    engine
        .store()
        .admissions(operation_id)
        .await
        .is_ok_and(|records| {
            admission_records_match(&records, operation_id, actor_claim, cardinality, true)
        })
}

fn release_transient_operation(
    engine: &SessionEngine,
    operation_id: hya_proto::OperationId,
    acquired: bool,
) {
    if acquired && let Some(governor) = engine.governor() {
        governor.release_operation(operation_id);
    }
}

struct ForegroundTransientAdmissionOwnerInit {
    engine: Arc<SessionEngine>,
    binding: TurnBinding,
    resolution: AdmissionResolutionContext,
    caller: String,
    allowed_agents: Arc<[hya_tool::AgentDef]>,
    sidecar_environment: Arc<BundleSidecarEnvironment>,
    request: SpawnRequest,
    root: SessionId,
    cardinality: u32,
    retained_intents: Vec<AdmissionIntent>,
    wake_router: Arc<ForegroundAdmissionWakeRouter>,
    reply_mode: DurableOwnerReplyMode,
}

struct ForegroundTransientAdmissionOwner {
    engine: Arc<SessionEngine>,
    binding: TurnBinding,
    resolution: AdmissionResolutionContext,
    caller: String,
    allowed_agents: Arc<[hya_tool::AgentDef]>,
    guidance: Option<Arc<str>>,
    sidecar_environment: Arc<BundleSidecarEnvironment>,
    parent: SessionId,
    root: SessionId,
    operation: hya_tool::ToolOperation,
    actor_claim: Option<hya_store::ActorClaim>,
    cardinality: u32,
    cancel: tokio_util::sync::CancellationToken,
    reply: Option<tokio::sync::oneshot::Sender<Result<Vec<MemberOutcome>, SpawnError>>>,
    debit_acquired: bool,
    authoritative_ordinals: BTreeSet<u32>,
    retained_intents: Vec<AdmissionIntent>,
    evidence: Vec<Option<MemberEvidence>>,
    scheduled: BTreeSet<u32>,
    handles: Vec<TransientAdmissionHandle>,
    completion_tx: Option<TransientAdmissionCompletionSender>,
    completion_rx: TransientAdmissionCompletionReceiver,
    closed: bool,
    wake_router: Arc<ForegroundAdmissionWakeRouter>,
    wake_registration: Option<ForegroundAdmissionWakeRegistration>,
    wake_rx: Option<tokio::sync::mpsc::Receiver<ForegroundAdmissionWake>>,
    reply_mode: DurableOwnerReplyMode,
    #[cfg(test)]
    test_observer: Option<Arc<AdmissionTestObserver>>,
    #[cfg(test)]
    uniform_probe: Option<Arc<ForegroundHandlerUniformProbe>>,
}

impl ForegroundTransientAdmissionOwner {
    fn new(init: ForegroundTransientAdmissionOwnerInit) -> Self {
        let ForegroundTransientAdmissionOwnerInit {
            engine,
            binding,
            resolution,
            caller,
            allowed_agents,
            sidecar_environment,
            request,
            root,
            cardinality,
            retained_intents,
            wake_router,
            reply_mode,
        } = init;
        let (completion_tx, completion_rx) =
            tokio::sync::mpsc::channel(usize::try_from(cardinality).unwrap_or(1).max(1));
        let operation = request.operation;
        let actor_claim = operation.actor_claim();
        let evidence = vec![None; usize::try_from(cardinality).unwrap_or(0)];
        let (wake_registration, wake_rx) = match wake_router.register(operation.operation_id()) {
            Some((registration, receiver)) => (Some(registration), Some(receiver)),
            None => (None, None),
        };
        #[cfg(test)]
        let test_observer = sidecar_environment.test_observer.clone();
        #[cfg(test)]
        let uniform_probe = sidecar_environment.uniform_probe.clone();
        #[cfg(test)]
        if let Some(observer) = &test_observer {
            observer.mark_owner_acquired(operation.operation_id());
        }
        Self {
            engine,
            binding,
            resolution,
            caller,
            allowed_agents,
            guidance: request.guidance,
            sidecar_environment,
            parent: request.parent,
            root,
            operation,
            actor_claim,
            cardinality,
            cancel: request.cancel,
            reply: Some(request.reply),
            debit_acquired: false,
            authoritative_ordinals: (0..cardinality).collect(),
            retained_intents,
            evidence,
            scheduled: BTreeSet::new(),
            handles: Vec::new(),
            completion_tx: Some(completion_tx),
            completion_rx,
            closed: false,
            wake_router,
            wake_registration,
            wake_rx,
            reply_mode,
            #[cfg(test)]
            test_observer,
            #[cfg(test)]
            uniform_probe,
        }
    }

    fn operation_id(&self) -> hya_proto::OperationId {
        self.operation.operation_id()
    }

    fn validate_launch(&self, launch: &AdmissionLaunch) -> Result<u32, &'static str> {
        let ordinal = launch.record.member_ordinal;
        if self.closed
            || launch.record.operation_id != self.operation_id()
            || launch.record.batch_size != self.cardinality
            || launch.record.state != hya_store::AdmissionState::Accepted
            || launch.record.actor
                != self
                    .actor_claim
                    .map(|claim| hya_store::AdmissionActorBinding {
                        actor_id: claim.actor_id,
                        actor_epoch: claim.epoch,
                    })
            || !self.authoritative_ordinals.contains(&ordinal)
            || self.scheduled.contains(&ordinal)
        {
            return Err("invalid transient admission launch");
        }
        Ok(ordinal)
    }

    async fn schedule(
        &mut self,
        mut resolved: ResolvedAdmissionLaunch,
    ) -> Result<(), &'static str> {
        let ordinal = self.validate_launch(&resolved.launch)?;
        self.scheduled.insert(ordinal);
        let completion = self
            .completion_tx
            .as_ref()
            .ok_or("transient admission owner is closed")?
            .clone();
        if self.reply_mode == DurableOwnerReplyMode::BackgroundRunningOnRegister {
            // Consult30 order: Started CAS → register session → reply running → work.
            let launch = resolved.launch.clone();
            let barrier = install_admission_task(&launch, async { Ok::<(), SpawnError>(()) });
            let barrier_handle = barrier
                .start(self.engine.store(), self.actor_claim.as_ref())
                .await
                .map_err(|_| "failed to start admitted member")?;
            let _ = barrier_handle.await;
            let create = CreateSession {
                parent: Some(self.parent),
                agent: resolved.resolved.agent.name.clone(),
                model: resolved.resolved.agent.model.clone(),
                workdir: resolved
                    .resolved
                    .agent
                    .workdir
                    .to_string_lossy()
                    .into_owned(),
            };
            let session = match self.actor_claim.as_ref() {
                Some(claim) => self
                    .engine
                    .create_for_actor(claim, create)
                    .await
                    .map_err(|_| "failed to register background session")?,
                None => self
                    .engine
                    .create(create)
                    .await
                    .map_err(|_| "failed to register background session")?,
            };
            resolved.resolved.request.task_id = Some(session.to_string());
            let member_id = MemberId::new();
            if let Some(reply) = self.reply.take() {
                let _ = reply.send(Ok(vec![MemberOutcome {
                    member: member_id.to_string(),
                    session: session.to_string(),
                    status: "running".to_string(),
                    summary: "The task is working in the background.".to_string(),
                }]));
            }
            let work_install = install_transient_admission_launch_with_completion(
                self.engine.clone(),
                resolved,
                self.cancel.clone(),
                self.actor_claim,
                completion,
            );
            let handle = work_install
                .start_signal_only()
                .map_err(|_| "failed to arm background member work")?;
            #[cfg(test)]
            if let Some(probe) = &self.uniform_probe {
                probe
                    .real_member_task_installations
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            self.handles.push(handle);
            return Ok(());
        }

        let installed = install_transient_admission_launch_with_completion(
            self.engine.clone(),
            resolved,
            self.cancel.clone(),
            self.actor_claim,
            completion,
        );
        let handle = installed
            .start(self.engine.store(), self.actor_claim.as_ref())
            .await
            .map_err(|_| "failed to start admitted member")?;
        #[cfg(test)]
        if let Some(probe) = &self.uniform_probe {
            probe
                .real_member_task_installations
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        self.handles.push(handle);
        Ok(())
    }

    async fn promote(&mut self, launch: AdmissionLaunch) -> Result<(), &'static str> {
        self.validate_launch(&launch)?;
        let context = AdmissionLaunchResolutionCtx {
            engine: &self.engine,
            binding: &self.binding,
            resolution: &self.resolution,
            caller: &self.caller,
            allowed_agents: self.allowed_agents.as_ref(),
            guidance: self.guidance.clone(),
            sidecar_environment: self.sidecar_environment.as_ref(),
        };
        let resolved =
            resolve_current_admission_launches(&context, launch, Some(self.wake_router.as_ref()))
                .await
                .map_err(|_| "promoted admission launch failed current validation")?;
        if resolved.is_empty() {
            return Err("promoted admission launch was not currently accepted");
        }
        for resolved in resolved {
            self.schedule(resolved).await?;
        }
        Ok(())
    }

    /// Reread durable journal rows and start any currently Accepted ordinals
    /// that this owner has not yet scheduled. Uses retained claim-time intents
    /// only — no store launch-read API.
    async fn rehydrate_accepted(&mut self) -> Result<(), &'static str> {
        let records = self
            .engine
            .store()
            .admissions(self.operation_id())
            .await
            .map_err(|_| "admission journal read failed")?;
        if !admission_records_match(
            &records,
            self.operation_id(),
            self.actor_claim.as_ref(),
            self.cardinality,
            false,
        ) {
            return Err("admission journal identity mismatch");
        }
        for record in records {
            if record.state != hya_store::AdmissionState::Accepted
                || self.scheduled.contains(&record.member_ordinal)
            {
                continue;
            }
            let ordinal = record.member_ordinal;
            let Some(intent) = usize::try_from(ordinal)
                .ok()
                .and_then(|index| self.retained_intents.get(index).cloned())
            else {
                return Err("missing retained admission intent");
            };
            let launch = AdmissionLaunch { record, intent };
            self.promote(launch).await?;
        }
        Ok(())
    }

    async fn accept_completion(
        &mut self,
        completion: TransientAdmissionCompletion,
    ) -> Result<(), &'static str> {
        let ordinal = completion.member_ordinal;
        let Some(index) = usize::try_from(ordinal)
            .ok()
            .filter(|index| *index < self.evidence.len())
        else {
            return Err("admitted member completion ordinal out of bounds");
        };
        if completion.operation_id != self.operation_id()
            || !self.scheduled.contains(&ordinal)
            || self.evidence[index].is_some()
        {
            return Err("duplicate or mismatched admitted member completion");
        }
        self.evidence[index] = Some(completion.evidence);
        for launch in completion.promoted {
            if launch.record.operation_id == self.operation_id() {
                self.promote(launch).await?;
            } else {
                // Foreign promotions are wake-only; the owning handler rehydrates.
                self.wake_router.wake(launch.record.operation_id);
            }
        }
        Ok(())
    }

    /// Consult30 cancel-first: durable Cancelled for nonterminal rows before
    /// activation, zero remaining launch allocation, one Cancelled error reply.
    async fn cancel_queued_before_activation(&mut self) {
        self.closed = true;
        self.completion_tx.take();
        let records = match self.engine.store().admissions(self.operation_id()).await {
            Ok(records) => records,
            Err(_) => {
                if let Some(reply) = self.reply.take() {
                    let _ = reply.send(Err(SpawnError::Cancelled));
                }
                return;
            }
        };
        let pending: Vec<_> = records
            .iter()
            .filter(|record| !record.state.is_terminal())
            .map(|record| (record.operation_id, record.member_ordinal))
            .collect();
        if !pending.is_empty() {
            let outcome = self
                .engine
                .store()
                .finalize_admission_members(
                    &pending,
                    AdmissionTerminal::Cancelled,
                    "spawn cancelled before activation",
                    self.actor_claim.as_ref(),
                )
                .await;
            if let Ok(outcome) = outcome {
                for promoted in outcome.promoted {
                    if promoted.record.operation_id != self.operation_id() {
                        self.wake_router.wake(promoted.record.operation_id);
                    }
                }
            }
        }
        // Abort any already-started handles without claiming a second reply.
        self.cancel.cancel();
        for handle in self.handles.drain(..) {
            handle.abort();
            let _ = handle.await;
        }
        release_transient_operation(&self.engine, self.operation_id(), self.debit_acquired);
        self.debit_acquired = false;
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(Err(SpawnError::Cancelled));
        }
    }

    async fn fail_after_claim(&mut self, reason: &str, error: SpawnError) {
        self.closed = true;
        self.completion_tx.take();
        #[cfg(test)]
        if let Some(observer) = &self.test_observer {
            observer.mark_cleanup_attempt();
        }
        let cleanup_proven = cleanup_transient_admission(
            &self.engine,
            self.operation_id(),
            self.actor_claim.as_ref(),
            &self.cancel,
            &mut self.handles,
            self.cardinality,
            reason,
            Some(self.wake_router.as_ref()),
        )
        .await;
        #[cfg(test)]
        if let Some(observer) = &self.test_observer {
            observer.mark_cleanup_finished();
        }
        if cleanup_proven {
            release_transient_operation(&self.engine, self.operation_id(), self.debit_acquired);
            self.debit_acquired = false;
            if let Some(reply) = self.reply.take() {
                let _ = reply.send(Err(error));
            }
        } else {
            std::future::pending::<()>().await;
        }
    }

    async fn quiesce_success(&mut self) {
        self.completion_tx.take();
        for handle in self.handles.drain(..) {
            let _ = handle.await;
        }
    }

    async fn run(mut self, initial: Vec<AdmissionLaunch>) {
        if self.wake_registration.is_none() || self.wake_rx.is_none() {
            self.fail_after_claim(
                "foreground admission wake route unavailable",
                SpawnError::Unavailable,
            )
            .await;
            return;
        }
        #[cfg(test)]
        if let Some(probe) = &self.uniform_probe {
            probe.owner_run_entered.notify_one();
            probe.owner_run_release.notified().await;
            if self.cancel.is_cancelled() {
                return;
            }
        }
        self.debit_acquired = match self.engine.governor() {
            Some(governor) => match governor.try_reserve_operation(
                self.root,
                self.operation_id(),
                u64::from(self.cardinality),
                self.cancel.clone(),
            ) {
                OperationReservation::Acquired => true,
                OperationReservation::Overloaded => {
                    self.fail_after_claim("spawn admission overloaded", SpawnError::Overloaded)
                        .await;
                    return;
                }
                OperationReservation::Existing | OperationReservation::Conflict => {
                    self.fail_after_claim(
                        "spawn admission operation already handled",
                        SpawnError::OperationAlreadyHandled,
                    )
                    .await;
                    return;
                }
            },
            None => false,
        };

        let context = AdmissionLaunchResolutionCtx {
            engine: &self.engine,
            binding: &self.binding,
            resolution: &self.resolution,
            caller: &self.caller,
            allowed_agents: self.allowed_agents.as_ref(),
            guidance: self.guidance.clone(),
            sidecar_environment: self.sidecar_environment.as_ref(),
        };
        let initial = match resolve_admission_launches(&context, initial) {
            Ok(initial) => initial,
            Err(_) => {
                self.fail_after_claim("admission resolution unavailable", SpawnError::Unavailable)
                    .await;
                return;
            }
        };
        for resolved in initial {
            if let Err(reason) = self.schedule(resolved).await {
                self.fail_after_claim(reason, SpawnError::Unavailable).await;
                return;
            }
        }
        // Immediate reread closes pre-registration races with foreign promotion.
        if let Err(reason) = self.rehydrate_accepted().await {
            self.fail_after_claim(reason, SpawnError::Unavailable).await;
            return;
        }

        let Some(mut wake_rx) = self.wake_rx.take() else {
            self.fail_after_claim(
                "foreground admission wake route unavailable",
                SpawnError::Unavailable,
            )
            .await;
            return;
        };
        while self.evidence.iter().any(Option::is_none) {
            // Background may have already replied running and still wait for
            // terminal journal convergence; cancellation only applies while the
            // caller oneshot is still held (pre-activation).
            tokio::select! {
                biased;
                _ = self.cancel.cancelled(), if self.reply.is_some() => {
                    self.cancel_queued_before_activation().await;
                    return;
                }
                message = self.completion_rx.recv() => {
                    let Some(message) = message else {
                        self.fail_after_claim(
                            "admitted member completion missing",
                            SpawnError::Unavailable,
                        )
                        .await;
                        return;
                    };
                    let completion = match message {
                        Ok(completion) => completion,
                        Err(_) => {
                            self.fail_after_claim(
                                "admitted member completion failed",
                                SpawnError::Unavailable,
                            )
                            .await;
                            return;
                        }
                    };
                    if let Err(reason) = self.accept_completion(completion).await {
                        self.fail_after_claim(reason, SpawnError::Unavailable).await;
                        return;
                    }
                }
                wake = wake_rx.recv() => {
                    if wake.is_none() {
                        self.fail_after_claim(
                            "foreground admission wake route closed",
                            SpawnError::Unavailable,
                        )
                        .await;
                        return;
                    }
                    if let Err(reason) = self.rehydrate_accepted().await {
                        self.fail_after_claim(reason, SpawnError::Unavailable).await;
                        return;
                    }
                }
            }
        }

        self.quiesce_success().await;
        let records = match self.engine.store().admissions(self.operation_id()).await {
            Ok(records)
                if admission_records_match(
                    &records,
                    self.operation_id(),
                    self.actor_claim.as_ref(),
                    self.cardinality,
                    true,
                ) =>
            {
                records
            }
            _ => {
                self.fail_after_claim(
                    "admission journal is not durably terminal",
                    SpawnError::Unavailable,
                )
                .await;
                return;
            }
        };
        let Some(evidence) = std::mem::take(&mut self.evidence)
            .into_iter()
            .collect::<Option<Vec<_>>>()
        else {
            self.fail_after_claim(
                "admitted member evidence is incomplete",
                SpawnError::Unavailable,
            )
            .await;
            return;
        };
        debug_assert_eq!(records.len(), evidence.len());
        let envelope = TeamEvidenceEnvelope {
            members: evidence.clone(),
        };
        let projected = match self.actor_claim.as_ref() {
            Some(claim) => {
                project_envelope_for_actor(&self.engine, self.parent, &envelope, claim).await
            }
            None => project_envelope(&self.engine, self.parent, &envelope).await,
        };
        if projected.is_err() {
            self.fail_after_claim("team evidence projection failed", SpawnError::Unavailable)
                .await;
            return;
        }
        let outcomes = evidence
            .into_iter()
            .map(|evidence| MemberOutcome {
                member: evidence.member,
                session: evidence.session,
                status: match evidence.status {
                    MemberStatus::Done => "done".to_string(),
                    MemberStatus::Failed => "failed".to_string(),
                },
                summary: evidence.summary,
            })
            .collect();
        release_transient_operation(&self.engine, self.operation_id(), self.debit_acquired);
        self.debit_acquired = false;
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(Ok(outcomes));
        }
    }
}

struct ForegroundTransientAdmissionPreparation {
    engine: Arc<SessionEngine>,
    binding: TurnBinding,
    base: AgentSpec,
    router: Arc<ProviderRouter>,
    categories: Arc<CategoryRegistry>,
    sidecar_environment: Arc<BundleSidecarEnvironment>,
    caller: String,
    req: SpawnRequest,
    wake_router: Arc<ForegroundAdmissionWakeRouter>,
    reply_mode: DurableOwnerReplyMode,
}

impl ForegroundTransientAdmissionPreparation {
    async fn run(self) {
        let Self {
            engine,
            binding,
            base,
            router,
            categories,
            sidecar_environment,
            caller,
            req,
            wake_router,
            reply_mode,
        } = self;
        #[cfg(test)]
        let watched_preparation = sidecar_environment
            .uniform_probe
            .as_ref()
            .is_some_and(|probe| probe.mark_preparation_entered(req.operation.operation_id()));
        #[cfg(test)]
        if let Some(probe) = &sidecar_environment.uniform_probe {
            probe.prepare_entered.notify_one();
            let release = if watched_preparation {
                probe.watched_preparation_release.notified()
            } else {
                probe.prepare_release.notified()
            };
            let mut release = Box::pin(release);
            let mut parked = false;
            std::future::poll_fn(|cx| match release.as_mut().poll(cx) {
                std::task::Poll::Ready(()) => std::task::Poll::Ready(()),
                std::task::Poll::Pending => {
                    if !parked {
                        probe.preparation_acquisitions.add_permits(1);
                        parked = true;
                    }
                    std::task::Poll::Pending
                }
            })
            .await;
        }
        let operation_id = req.operation.operation_id();
        let actor_claim = req.operation.actor_claim();
        let cardinality = match u32::try_from(req.members.len()) {
            Ok(cardinality) if cardinality > 0 => cardinality,
            _ => {
                let _ = req.reply.send(Err(SpawnError::Overloaded));
                return;
            }
        };
        let prepared = match prepare_spawn_admission(
            &engine, &binding, base, categories, router, &caller, &req,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = req.reply.send(Err(error));
                return;
            }
        };
        let (root, depth) = match engine.session_lineage(req.parent).await {
            Ok(lineage) => lineage,
            Err(_) => {
                let _ = req.reply.send(Err(SpawnError::Unavailable));
                return;
            }
        };
        if engine
            .governor()
            .is_some_and(|governor| depth.saturating_add(1) > governor.max_depth())
        {
            let _ = req.reply.send(Err(SpawnError::Overloaded));
            return;
        }
        let claim = AdmissionClaim {
            operation_id,
            source_tool_call_id: req.operation.source_tool_call_id(),
            root_session: root,
            request_fingerprint: prepared.request_fingerprint,
            admission_units: cardinality,
            actor_claim,
        };
        let PreparedSpawnAdmission {
            request_fingerprint: _,
            resolution,
            intents,
        } = prepared;
        let retained_intents = intents.clone();
        #[cfg(test)]
        if let Some(probe) = &sidecar_environment.uniform_probe {
            probe.before_claim.notify_one();
            probe.before_claim_release.notified().await;
        }
        let launches = match engine.store().claim_admission_batch(&claim, intents).await {
            Ok(AdmissionBatchClaimOutcome::Claimed(launches)) => launches,
            Ok(AdmissionBatchClaimOutcome::Existing) => {
                let _ = req.reply.send(Err(SpawnError::OperationAlreadyHandled));
                return;
            }
            Err(StoreError::AdmissionCapacityExceeded { .. }) => {
                let _ = req.reply.send(Err(SpawnError::Overloaded));
                return;
            }
            Err(StoreError::OperationIdConflict { .. }) => {
                let _ = req.reply.send(Err(SpawnError::OperationIdConflict));
                return;
            }
            Err(_) => {
                let _ = req.reply.send(Err(SpawnError::Unavailable));
                return;
            }
        };
        #[cfg(test)]
        if let Some(probe) = &sidecar_environment.uniform_probe {
            probe.after_claim.notify_one();
            probe.after_claim_release.notified().await;
        }
        ForegroundTransientAdmissionOwner::new(ForegroundTransientAdmissionOwnerInit {
            engine,
            binding,
            resolution,
            caller,
            allowed_agents: req.agents.clone(),
            sidecar_environment,
            request: req,
            root,
            cardinality,
            retained_intents,
            wake_router,
            reply_mode,
        })
        .run(launches)
        .await;
    }
}

/// Private supervisor ownership for foreground admission handlers.
///
/// Explicit [`shutdown`](SpawnSupervisorLifecycle::shutdown) drains handlers.
/// [`Drop`] is nonblocking: it only signals stop and aborts the supervisor task.
struct SpawnSupervisorLifecycle {
    stop: tokio_util::sync::CancellationToken,
    join: Option<tokio::task::JoinHandle<()>>,
    /// Adjacent worker loops sharing the same stop token (workflow runs).
    extra_joins: Vec<tokio::task::JoinHandle<()>>,
}

impl SpawnSupervisorLifecycle {
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.stop.cancel();
        if let Some(join) = self.join.take() {
            match join.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {
                    return Err(CoreError::Invalid(
                        "spawn supervisor cancelled during shutdown".to_string(),
                    ));
                }
                Err(error) => {
                    return Err(CoreError::Invalid(format!(
                        "spawn supervisor failed during shutdown: {error}"
                    )));
                }
            }
        }
        for join in self.extra_joins.drain(..) {
            match join.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {
                    return Err(CoreError::Invalid(
                        "workflow supervisor cancelled during shutdown".to_string(),
                    ));
                }
                Err(error) => {
                    return Err(CoreError::Invalid(format!(
                        "workflow supervisor failed during shutdown: {error}"
                    )));
                }
            }
        }
        Ok(())
    }

    fn drop_stop(&mut self) {
        self.stop.cancel();
        if let Some(join) = self.join.take() {
            join.abort();
        }
        for join in self.extra_joins.drain(..) {
            join.abort();
        }
    }
}

impl Drop for SpawnSupervisorLifecycle {
    fn drop(&mut self) {
        self.drop_stop();
    }
}

/// Public aggregate returned by [`build_session_engine`].
///
/// Owns engine products plus the private spawn-supervisor lifecycle. Not `Clone`.
/// Callers must keep this value until they finish using the engine and then call
/// [`shutdown`](BuiltSessionEngine::shutdown) (or drop for a nonblocking fallback).
#[must_use]
pub struct BuiltSessionEngine {
    engine: Arc<SessionEngine>,
    resident_supervisor: Arc<ResidentSupervisor>,
    asks: Option<tokio::sync::mpsc::UnboundedReceiver<AskRequest>>,
    questions: Option<tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>>,
    mcp_control: Arc<dyn hya_server::McpControl>,
    plugin_host: Arc<hya_plugin::PluginHost>,
    lifecycle: SpawnSupervisorLifecycle,
}

impl BuiltSessionEngine {
    /// Shared session engine handle.
    #[must_use]
    pub fn engine(&self) -> Arc<SessionEngine> {
        Arc::clone(&self.engine)
    }

    /// Shared resident scheduling owner used by Workflow actor activations.
    #[must_use]
    pub fn resident_supervisor(&self) -> Arc<ResidentSupervisor> {
        Arc::clone(&self.resident_supervisor)
    }

    /// Take the permission-ask receiver exactly once.
    pub fn take_asks(&mut self) -> Option<tokio::sync::mpsc::UnboundedReceiver<AskRequest>> {
        self.asks.take()
    }

    /// Take the interaction-question receiver exactly once.
    pub fn take_questions(
        &mut self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>> {
        self.questions.take()
    }

    /// MCP control plane handle.
    #[must_use]
    pub fn mcp_control(&self) -> Arc<dyn hya_server::McpControl> {
        Arc::clone(&self.mcp_control)
    }

    /// Plugin host handle.
    #[must_use]
    pub fn plugin_host(&self) -> Arc<hya_plugin::PluginHost> {
        Arc::clone(&self.plugin_host)
    }

    /// Stop intake, abort handlers, and drain the supervisor JoinSet.
    pub async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.lifecycle.shutdown().await
    }
}

impl Drop for BuiltSessionEngine {
    fn drop(&mut self) {
        self.lifecycle.drop_stop();
    }
}

/// Run a body against a built engine and always attempt supervisor shutdown.
pub async fn with_built_session_engine<T, E, F, Fut>(
    mut built: BuiltSessionEngine,
    body: F,
) -> Result<T, E>
where
    F: FnOnce(&mut BuiltSessionEngine) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: From<CoreError>,
{
    let body_result = body(&mut built).await;
    let shutdown_result = built.shutdown().await.map_err(E::from);
    match (body_result, shutdown_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Spawn the background task that admits multi-agent team/member spawn requests.
///
/// Consumes `rx` for the lifetime of the process (or until the channel closes).
pub fn spawn_team_supervisor(
    rx: tokio::sync::mpsc::Receiver<BoundSpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    router: Arc<ProviderRouter>,
    categories: Arc<CategoryRegistry>,
    resident_supervisor: Arc<ResidentSupervisor>,
) {
    let _lifecycle = spawn_team_supervisor_with_environment(
        rx,
        engine,
        base,
        router,
        categories,
        resident_supervisor,
        Arc::new(BundleSidecarEnvironment::production()),
    );
    // Test/helper entry: lifecycle is intentionally detached; production uses
    // BuiltSessionEngine ownership. Drop still nonblocking-aborts on process end.
    std::mem::forget(_lifecycle);
}

/// Serve queued `workflow` tool requests for the lifetime of the engine.
///
/// One worker loop, one in-flight run: user DAGs are long-lived multi-agent
/// executions and the governor's per-run budget already bounds each run's total
/// fan-out, so extra intra-process parallelism would only add contention.
fn spawn_workflow_supervisor(
    mut rx: tokio::sync::mpsc::Receiver<BoundWorkflowRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    resident_supervisor: Arc<ResidentSupervisor>,
    stop: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let bound_request = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                bound_request = rx.recv() => match bound_request {
                    Some(request) => request,
                    None => break,
                },
            };
            let (binding, request) = bound_request.into_parts();
            let request_cancel = request.cancel.clone();
            let execution =
                handle_workflow_request(&engine, &base, &resident_supervisor, binding, request);
            tokio::pin!(execution);
            tokio::select! {
                biased;
                _ = stop.cancelled() => {
                    // Shutdown must not wait forever behind a stalled provider or
                    // verifier. Cancel cooperatively, then await the governed run
                    // so it can finish member/session cleanup before this worker exits.
                    request_cancel.cancel();
                    execution.await;
                    break;
                }
                _ = &mut execution => {}
            }
        }
    })
}

/// Execute or discover one framed workflow request against the caller session.
async fn handle_workflow_request(
    engine: &Arc<SessionEngine>,
    base: &AgentSpec,
    resident_supervisor: &Arc<ResidentSupervisor>,
    binding: TurnBinding,
    request: hya_tool::WorkflowRequest,
) {
    let hya_tool::WorkflowRequest {
        parent,
        action,
        cancel,
        reply,
        ..
    } = request;
    let caller = match engine.read_projection(parent).await {
        Ok(projection) => projection.session.agent.map(|agent| agent.to_string()),
        Err(error) => {
            let _ = reply.send(Err(format!("resolve caller session: {error}")));
            return;
        }
    };
    let Some(caller) = caller else {
        let _ = reply.send(Err("workflow caller session has no agent".to_string()));
        return;
    };
    let result = match action {
        hya_tool::WorkflowAction::List => list_workflows(binding.workdir()),
        hya_tool::WorkflowAction::Run { name, inputs } => {
            run_named_workflow(
                engine,
                base,
                resident_supervisor,
                &binding,
                parent,
                &caller,
                &name,
                inputs,
                cancel,
            )
            .await
        }
    };
    let _ = reply.send(result);
}

/// Discover workflows across the workdir roots; earlier (shadowing) wins on
/// duplicate names, mirroring `load_workflow_by_name` resolution order.
fn list_workflows(workdir: &std::path::Path) -> hya_tool::WorkflowReply {
    use hya_tool::{WorkflowReplyPayload, WorkflowSummary};
    let mut summaries: Vec<WorkflowSummary> = Vec::new();
    for path in discover_workflow_files(workdir) {
        let display = path.display().to_string();
        match load_workflow_file(&path) {
            Ok(workflow) => {
                let definition = workflow.definition();
                if summaries
                    .iter()
                    .any(|summary| summary.name == definition.name())
                {
                    continue;
                }
                summaries.push(WorkflowSummary {
                    name: definition.name().to_string(),
                    description: definition.description().to_string(),
                    path: display,
                    stages: workflow
                        .plan()
                        .stages()
                        .iter()
                        .map(|stage| stage.id().to_string())
                        .collect(),
                    error: None,
                });
            }
            Err(error) => summaries.push(WorkflowSummary {
                name: path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                description: String::new(),
                path: display,
                stages: Vec::new(),
                error: Some(error.to_string()),
            }),
        }
    }
    Ok(WorkflowReplyPayload::List(summaries))
}

/// Load, authorize, and execute one named workflow through the governed core
/// executor (`run_workflow` internally uses the pre-admitted team batch path).
#[allow(clippy::too_many_arguments)]
async fn run_named_workflow(
    engine: &Arc<SessionEngine>,
    base: &AgentSpec,
    resident_supervisor: &Arc<ResidentSupervisor>,
    binding: &TurnBinding,
    lead: hya_proto::SessionId,
    caller: &str,
    name: &str,
    inputs: BTreeMap<String, String>,
    cancel: tokio_util::sync::CancellationToken,
) -> hya_tool::WorkflowReply {
    use hya_tool::{WorkflowOutcome, WorkflowReplyPayload, WorkflowStageOutcome};
    let def = match load_workflow_by_name(std::path::Path::new(binding.workdir()), name) {
        Ok(def) => def,
        Err(error) => return Err(error.to_string()),
    };
    let context = hya_core::WorkflowRunContext {
        binding: binding.clone(),
        caller: caller.to_string(),
        base_agent: base.clone(),
        inputs,
        resident_supervisor: Some(resident_supervisor.clone()),
    };
    let report = match run_workflow(engine.clone(), lead, &def, context, cancel).await {
        Ok(report) => report,
        Err(error) => return Err(error.to_string()),
    };
    Ok(WorkflowReplyPayload::Run(WorkflowOutcome {
        status: report.status.to_string(),
        stages: report
            .stages
            .iter()
            .map(|stage| WorkflowStageOutcome {
                stage: stage.stage.clone(),
                agent: stage.agent.clone(),
                status: stage.status.to_string(),
                output: stage.output.clone(),
            })
            .collect(),
    }))
}

fn observe_foreground_handler_join(joined: Option<Result<(), tokio::task::JoinError>>) {
    if let Some(Err(error)) = joined {
        eprintln!("hya: foreground spawn handler failed ({error})");
    }
}

const FOREGROUND_HANDLER_CAP: usize = 256;

fn spawn_team_supervisor_with_environment(
    mut rx: tokio::sync::mpsc::Receiver<BoundSpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    router: Arc<ProviderRouter>,
    categories: Arc<CategoryRegistry>,
    resident_supervisor: Arc<ResidentSupervisor>,
    sidecar_environment: Arc<BundleSidecarEnvironment>,
) -> SpawnSupervisorLifecycle {
    let wake_router = Arc::new(ForegroundAdmissionWakeRouter::new(
        #[cfg(test)]
        sidecar_environment.test_observer.clone(),
    ));
    let stop = tokio_util::sync::CancellationToken::new();
    let stop_child = stop.child_token();
    let join = tokio::spawn(async move {
        let mut foreground_handlers = tokio::task::JoinSet::new();
        loop {
            if stop_child.is_cancelled() {
                foreground_handlers.abort_all();
                while let Some(joined) = foreground_handlers.join_next().await {
                    observe_foreground_handler_join(Some(joined));
                }
                break;
            }
            let bound_request = loop {
                if stop_child.is_cancelled() {
                    break None;
                }
                #[cfg(test)]
                if foreground_handlers.len() == FOREGROUND_HANDLER_CAP
                    && let Some(probe) = &sidecar_environment.uniform_probe
                {
                    probe.supervisor_full_observed.add_permits(1);
                }
                if foreground_handlers.len() >= FOREGROUND_HANDLER_CAP {
                    tokio::select! {
                        biased;
                        _ = stop_child.cancelled() => break None,
                        joined = foreground_handlers.join_next() => {
                            observe_foreground_handler_join(joined);
                        }
                    }
                    continue;
                }
                if foreground_handlers.is_empty() {
                    tokio::select! {
                        biased;
                        _ = stop_child.cancelled() => break None,
                        bound_request = rx.recv() => break bound_request,
                    }
                }
                tokio::select! {
                    biased;
                    _ = stop_child.cancelled() => break None,
                    bound_request = rx.recv() => break bound_request,
                    joined = foreground_handlers.join_next() => {
                        observe_foreground_handler_join(joined);
                    }
                }
            };
            let Some(bound_request) = bound_request else {
                // This branch is reached two ways: the stop token was cancelled
                // (explicit shutdown), or intake closed because the last
                // `BoundSpawnSender` was dropped. Only shutdown aborts in-flight
                // handlers; a closed intake must drain already-admitted work to
                // completion, or its reply oneshot is dropped and the caller sees
                // a spurious `SpawnError::Unavailable`.
                //
                // The drain below deliberately does not watch `stop_child`. That is
                // safe only because of the spawn-intake liveness invariant recorded
                // at the `with_spawn_sender` call site: in production the engine
                // owns the sender and this task owns an `Arc<SessionEngine>`, so a
                // closed intake is unreachable and this branch is entered only via
                // cancellation, which has already aborted the handlers. The closed
                // -intake path is exercised only by the `spawn_team_supervisor`
                // test helper. Were intake ever able to close with handlers in
                // flight, a later `shutdown()` would block here rather than abort --
                // `fail_after_claim` can park on `std::future::pending::<()>()` --
                // so that invariant must be rechecked before changing this.
                if stop_child.is_cancelled() {
                    foreground_handlers.abort_all();
                }
                while let Some(joined) = foreground_handlers.join_next().await {
                    observe_foreground_handler_join(Some(joined));
                }
                break;
            };
            let (binding, req) = bound_request.into_parts();
            let parent = match engine.read_projection(req.parent).await {
                Ok(parent) => parent,
                Err(error) => {
                    eprintln!("hya: failed to resolve spawn parent ({error})");
                    let _ = req.reply.send(Err(SpawnError::Unavailable));
                    continue;
                }
            };
            let Some(caller) = parent.session.agent.as_ref() else {
                let _ = req.reply.send(Err(SpawnError::Unavailable));
                continue;
            };
            if uses_durable_admission_owner(&binding, caller.as_str(), &req) {
                let handler_engine = Arc::clone(&engine);
                let handler_base = base.clone();
                let handler_router = Arc::clone(&router);
                let handler_categories = Arc::clone(&categories);
                let handler_sidecar_environment = Arc::clone(&sidecar_environment);
                let handler_wake_router = Arc::clone(&wake_router);
                let handler_caller = caller.as_str().to_string();
                let reply_mode = if req.background {
                    DurableOwnerReplyMode::BackgroundRunningOnRegister
                } else {
                    DurableOwnerReplyMode::ForegroundWholeBatch
                };
                foreground_handlers.spawn(async move {
                    #[cfg(test)]
                    let _probe_guard = handler_sidecar_environment
                        .uniform_probe
                        .as_ref()
                        .map(|probe| ForegroundHandlerProbeGuard::new(Arc::clone(probe)));
                    ForegroundTransientAdmissionPreparation {
                        engine: handler_engine,
                        binding,
                        base: handler_base,
                        router: handler_router,
                        categories: handler_categories,
                        sidecar_environment: handler_sidecar_environment,
                        caller: handler_caller,
                        req,
                        wake_router: handler_wake_router,
                        reply_mode,
                    }
                    .run()
                    .await;
                });
                continue;
            }
            let is_servable = |model: &ModelRef| router.resolve(model).is_some();
            // Guidance is request-scoped: one Arc cloned to every resolved member.
            let request_guidance = req.guidance.clone();
            // Scope the ctx so the non-Sync `is_servable` borrow ends before any await.
            let resolved = {
                let resolve_ctx = ResolveSpawnMemberCtx {
                    engine: &engine,
                    binding: &binding,
                    base: &base,
                    caller: caller.as_str(),
                    allowed_agents: &req.agents,
                    categories: &categories,
                    is_servable: &is_servable,
                    guidance: request_guidance.clone(),
                    sidecar_environment: sidecar_environment.as_ref(),
                };
                req.members
                    .iter()
                    .cloned()
                    .map(|member| resolve_spawn_member(&resolve_ctx, member))
                    .collect::<Result<Vec<_>, _>>()
            };
            let resolved = match resolved {
                Ok(resolved) => resolved,
                Err(error) => {
                    let _ = req.reply.send(Err(error));
                    continue;
                }
            };
            // Resident batches need team-root main activation context resolved from
            // the same captured TurnBinding before any durable admission. Nested
            // callers must not supply their own roster/resource policy for main.
            // Transient-only batches skip this lookup entirely.
            let batch_has_resident = resolved.iter().any(|entry| entry.resident);
            let main_activation = if batch_has_resident {
                match resolve_main_activation_context(
                    &engine,
                    &binding,
                    &base,
                    req.parent,
                    request_guidance.clone(),
                )
                .await
                {
                    Ok(context) => Some(context),
                    Err(error) => {
                        let _ = req.reply.send(Err(error));
                        continue;
                    }
                }
            } else {
                None
            };
            let fingerprint = match spawn_request_fingerprint(&req) {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    eprintln!("hya: failed to fingerprint spawn request ({error})");
                    let _ = req.reply.send(Err(SpawnError::Unavailable));
                    continue;
                }
            };
            let admission_units = u32::try_from(req.members.len()).unwrap_or(u32::MAX);
            let operation_id = req.operation.operation_id();
            let actor_claim = req.operation.actor_claim();
            // Anchors every spawn edge to the `task` call that produced it, so an
            // offline call graph does not have to infer it from event ordering.
            let source_tool_call = req.operation.source_tool_call_id();
            let admission = engine
                .begin_spawn_admission(
                    req.parent,
                    req.operation,
                    fingerprint,
                    admission_units,
                    actor_claim,
                    req.cancel.clone(),
                )
                .await;
            match admission {
                Ok(SpawnAdmissionOutcome::Started) => {}
                Ok(SpawnAdmissionOutcome::Overloaded | SpawnAdmissionOutcome::MaxDepth) => {
                    let _ = req.reply.send(Err(SpawnError::Overloaded));
                    continue;
                }
                Ok(SpawnAdmissionOutcome::Existing(_) | SpawnAdmissionOutcome::Cancelled) => {
                    let _ = req.reply.send(Err(SpawnError::OperationAlreadyHandled));
                    continue;
                }
                Err(CoreError::Store(StoreError::OperationIdConflict { .. })) => {
                    let _ = req.reply.send(Err(SpawnError::OperationIdConflict));
                    continue;
                }
                Err(error) => {
                    eprintln!("hya: durable spawn admission failed ({error})");
                    let _ = req.reply.send(Err(SpawnError::Unavailable));
                    continue;
                }
            }
            let engine = engine.clone();
            let resident_supervisor = resident_supervisor.clone();
            tokio::spawn(async move {
                let parent = req.parent;
                let operation_cancel = req.cancel;
                let cancel = operation_cancel.clone();
                let background = req.background;
                let mut reply = Some(req.reply);
                let mut spawn_failed = false;
                let mut resident_members = Vec::new();
                let mut transient_members = Vec::new();
                for entry in resolved {
                    if entry.resident {
                        resident_members.push(entry);
                    } else {
                        transient_members.push(entry);
                    }
                }

                // Resident members are NON-BLOCKING: register each as a long-lived
                // turn is not held on their work.
                let mut resident_outcomes = Vec::new();
                if !resident_members.is_empty() {
                    // Register the team root as the main actor so child mail +
                    // quiescence can wake it. Only done when the team actually has
                    // residents, so pure-transient teams keep their old behavior.
                    // Synthesis is an ordinary activation of this main slot through
                    // the same resident_task path, seeded with pre-admission root
                    // context (first ensure_main wins on re-entry).
                    let main_activation_result = if let Some(MainActivationContext {
                        root,
                        agent: main_agent,
                        binding: main_binding,
                        agents: main_agents,
                        resources: main_resources,
                        guidance: main_guidance,
                    }) = main_activation
                    {
                        resident_supervisor
                            .ensure_main(
                                root,
                                main_agent,
                                (main_binding, main_agents, main_resources),
                                actor_claim.as_ref(),
                                main_guidance,
                            )
                            .await
                    } else {
                        Ok(())
                    };
                    if let Err(err) = main_activation_result {
                        let summary = err.to_string();
                        eprintln!("hya: ensure_main failed ({summary})");
                        spawn_failed = true;
                        for _ in 0..resident_members.len() {
                            resident_outcomes.push(MemberOutcome {
                                member: "-".to_string(),
                                session: "-".to_string(),
                                status: "failed".to_string(),
                                summary: summary.clone(),
                            });
                        }
                    } else {
                        for resolved in resident_members {
                            let ResolvedSpawnMember {
                                request: member,
                                authorized_target,
                                agent,
                                binding,
                                agents,
                                resources,
                                guidance,
                                sidecar_factory,
                                ..
                            } = resolved;
                            let _authorized_target = authorized_target;
                            match resident_supervisor
                                .spawn_resident(
                                    parent,
                                    agent,
                                    (binding, agents, resources, sidecar_factory),
                                    member.prompt,
                                    actor_claim.as_ref(),
                                    guidance,
                                )
                                .await
                            {
                                Ok((session, handle)) => resident_outcomes.push(MemberOutcome {
                                    member: handle.clone(),
                                    session: session.to_string(),
                                    status: "running".to_string(),
                                    summary: format!(
                                        "Resident {handle} is live and will act on inbound mail."
                                    ),
                                }),
                                Err(err) => resident_outcomes.push(MemberOutcome {
                                    member: "-".to_string(),
                                    session: "-".to_string(),
                                    status: "failed".to_string(),
                                    summary: {
                                        spawn_failed = true;
                                        err.to_string()
                                    },
                                }),
                            }
                        }
                    }
                }

                // Transient members keep the historical blocking-join semantics.
                let specs: Vec<MemberSpec> = if background {
                    let mut specs = Vec::new();
                    let mut started = resident_outcomes.clone();
                    for resolved in transient_members {
                        let ResolvedSpawnMember {
                            request: member,
                            authorized_target,
                            agent,
                            binding,
                            agents,
                            resources,
                            guidance,
                            sidecar_factory,
                            ..
                        } = resolved;
                        let _authorized_target = authorized_target;
                        let id = MemberId::new();
                        let session = match member
                            .task_id
                            .as_deref()
                            .and_then(|task_id| task_id.parse::<SessionId>().ok())
                        {
                            Some(session) => session,
                            None => {
                                let create = CreateSession {
                                    parent: Some(parent),
                                    agent: agent.name.clone(),
                                    model: agent.model.clone(),
                                    workdir: agent.workdir.to_string_lossy().into_owned(),
                                };
                                let created = match actor_claim.as_ref() {
                                    Some(claim) => engine.create_for_actor(claim, create).await,
                                    None => engine.create(create).await,
                                };
                                match created {
                                    Ok(session) => session,
                                    Err(err) => {
                                        spawn_failed = true;
                                        started.push(MemberOutcome {
                                            member: id.to_string(),
                                            session: "-".to_string(),
                                            status: "failed".to_string(),
                                            summary: err.to_string(),
                                        });
                                        continue;
                                    }
                                }
                            }
                        };
                        started.push(MemberOutcome {
                            member: id.to_string(),
                            session: session.to_string(),
                            status: "running".to_string(),
                            summary: "The task is working in the background.".to_string(),
                        });
                        specs.push(MemberSpec {
                            id,
                            agent,
                            binding,
                            agents,
                            resources: Some(resources),
                            guidance,
                            directive: member.prompt,
                            description: member.description,
                            session: Some(session),
                            sidecar_factory,
                            tool_call: Some(source_tool_call),
                        });
                    }
                    if let Some(reply) = reply.take() {
                        let _ = reply.send(Ok(started));
                    }
                    specs
                } else {
                    transient_members
                        .into_iter()
                        .map(|resolved| {
                            let ResolvedSpawnMember {
                                request,
                                authorized_target,
                                agent,
                                binding,
                                agents,
                                resources,
                                guidance,
                                sidecar_factory,
                                ..
                            } = resolved;
                            let _authorized_target = authorized_target;
                            MemberSpec {
                                id: MemberId::new(),
                                agent,
                                binding,
                                agents,
                                resources: Some(resources),
                                guidance,
                                directive: request.prompt,
                                description: request.description,
                                session: request
                                    .task_id
                                    .as_deref()
                                    .and_then(|task_id| task_id.parse::<SessionId>().ok()),
                                sidecar_factory,
                                tool_call: Some(source_tool_call),
                            }
                        })
                        .collect()
                };

                // Only run the blocking join when there is transient work; a pure
                // resident spawn replies immediately with the resident handles.
                let mut outcomes = resident_outcomes;
                if !specs.is_empty() {
                    let evidence = match actor_claim {
                        Some(claim) => {
                            run_pre_admitted_team_for_actor(
                                engine.clone(),
                                parent,
                                specs,
                                cancel,
                                claim,
                            )
                            .await
                        }
                        None => run_pre_admitted_team(engine.clone(), parent, specs, cancel).await,
                    };
                    spawn_failed |= evidence
                        .iter()
                        .any(|member| member.status == MemberStatus::Failed);
                    let envelope = TeamEvidenceEnvelope {
                        members: evidence.clone(),
                    };
                    let _ = match actor_claim.as_ref() {
                        Some(claim) => {
                            project_envelope_for_actor(&engine, parent, &envelope, claim).await
                        }
                        None => project_envelope(&engine, parent, &envelope).await,
                    };
                    outcomes.extend(evidence.into_iter().map(|e| MemberOutcome {
                        member: e.member,
                        session: e.session,
                        status: match e.status {
                            MemberStatus::Done => "done".to_string(),
                            MemberStatus::Failed => "failed".to_string(),
                        },
                        summary: e.summary,
                    }));
                }
                let (terminal, reason) = if operation_cancel.is_cancelled() {
                    (AdmissionTerminal::Cancelled, "spawn operation cancelled")
                } else if spawn_failed {
                    (AdmissionTerminal::Aborted, "spawn operation failed")
                } else {
                    (AdmissionTerminal::Completed, "spawn operation completed")
                };
                if let Err(error) = engine
                    .finalize_spawn_admission(operation_id, terminal, reason, actor_claim.as_ref())
                    .await
                {
                    eprintln!("hya: failed to finalize spawn admission ({error})");
                    if !background && let Some(reply) = reply.take() {
                        let _ = reply.send(Err(SpawnError::Unavailable));
                    }
                    return;
                }
                if !background && let Some(reply) = reply.take() {
                    let _ = reply.send(Ok(outcomes));
                }
            });
        }
    });
    SpawnSupervisorLifecycle {
        stop,
        join: Some(join),
        extra_joins: Vec::new(),
    }
}

/// When true (default), MCP connect runs after the engine is built so HTTP can
/// listen without waiting on child process handshakes. Set `HYA_DEFER_SIDEPLANES=0`
/// to restore the classic await-before-listen path.
fn defer_sideplanes() -> bool {
    match std::env::var("HYA_DEFER_SIDEPLANES") {
        Ok(value) => {
            let text = value.trim();
            !(text.eq_ignore_ascii_case("0")
                || text.eq_ignore_ascii_case("false")
                || text.eq_ignore_ascii_case("off")
                || text.eq_ignore_ascii_case("no"))
        }
        Err(_) => true,
    }
}

#[derive(Clone, Copy)]
struct EngineBuildOptions {
    defer_mcp: bool,
}

/// Build a fully wired [`SessionEngine`] plus plugin host, MCP, and ask/question channels.
///
/// MCP connect deferral follows process policy (`defer_sideplanes`). Callers must
/// eventually shut down the returned [`BuiltSessionEngine`] (or use
/// [`with_built_session_engine`]).
pub async fn build_session_engine(
    store: SessionStore,
    router: ProviderRouter,
    agent: &AgentSpec,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
    tool_config: (WebSearchConfig, InvocationPolicy),
) -> anyhow::Result<BuiltSessionEngine> {
    build_session_engine_with_mcp_defer(
        store,
        router,
        agent,
        mcp,
        plugins,
        tool_config,
        EngineBuildOptions {
            defer_mcp: defer_sideplanes(),
        },
    )
    .await
}

/// Seed the engine's cross-model failover plane from configured categories.
///
/// Every candidate position seeds its forward suffix chain (`candidate[k..]`),
/// so members resolved onto a servability-picked candidate — not only the
/// configured preference — keep working failover for their whole lifetime.
/// Single-candidate categories contribute nothing (there is no cross-model
/// step). When two categories share a preferred model the first category in
/// canonical key order wins; identical configs collapse to the same chain.
fn category_model_fallbacks(categories: &CategoryRegistry) -> HashMap<ModelRef, Vec<ModelRef>> {
    let mut fallbacks: HashMap<ModelRef, Vec<ModelRef>> = HashMap::new();
    for (_, candidates) in categories.resolution_candidates() {
        for (offset, candidate) in candidates.iter().enumerate() {
            let forward = candidates[offset..].to_vec();
            if forward.len() > 1 {
                fallbacks.entry(candidate.clone()).or_insert(forward);
            }
        }
    }
    fallbacks
}

async fn build_session_engine_with_mcp_defer(
    store: SessionStore,
    router: ProviderRouter,
    agent: &AgentSpec,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
    tool_config: (WebSearchConfig, InvocationPolicy),
    options: EngineBuildOptions,
) -> anyhow::Result<BuiltSessionEngine> {
    let owner_run_id = process_owner_run_id();
    let mut recovered_claims = Vec::new();
    for actor_id in store
        .active_actor_ids()
        .await
        .context("list resident actors before startup recovery")?
    {
        recovered_claims.push(
            store
                .recover_claim(actor_id, owner_run_id)
                .await
                .context("fence resident actor before startup recovery")?,
        );
    }
    store
        .recover_nonterminal_admissions("startup recovery")
        .await
        .context("recover nonterminal admissions before spawn readiness")?;
    let (websearch, invocation_policy) = tool_config;
    let router = Arc::new(router);
    let registry = ToolRegistry::builtins();
    if !websearch.enabled {
        registry.remove("websearch");
    }

    // Plugin hooks remain startup-bound. Their tool declarations are prepared
    // here but become effective only through RuntimeReconciler publication.
    let plugin_specs = plugins.clone();
    let (plugin_host, plugin_failures) =
        hya_plugin::PluginHost::connect_all_observed(plugins, host_info()).await;
    let plugin_host = Arc::new(plugin_host);
    let prepared_plugins = plugin_host
        .prepared_plugins()
        .into_iter()
        .map(|plugin| (plugin.id().to_string(), plugin))
        .collect::<BTreeMap<_, _>>();
    let defer_mcp = options.defer_mcp && !mcp.is_empty();
    let catalog = builtin_agent_catalog()?;
    let runtime = Arc::new(RuntimeRegistry::new(registry, catalog));
    let catalog_refresh = Arc::new(InstalledBundleRefresh::new(bundle_registry_path()));

    let rules = PermissionRules::new(vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::Glob, "*", Mode::Allow),
        Rule::new(Action::Grep, "*", Mode::Allow),
    ]);
    let (permission, asks) = PermissionPlane::new_with_policy(rules, invocation_policy);
    let permission = if plugin_host.is_empty() {
        permission
    } else {
        permission.with_interceptor(Arc::new(hya_plugin::PermissionBridge::new(
            plugin_host.clone(),
        )))
    };
    let (interaction, questions) = InteractionPlane::new();
    let subagent_limits = crate::config::load_subagent_limits();
    let spawn_queue_capacity = usize::try_from(subagent_limits.per_run_budget)
        .unwrap_or(tokio::sync::Semaphore::MAX_PERMITS)
        .clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(spawn_queue_capacity);
    let (workflow_sender, workflow_rx) = BoundWorkflowSender::with_capacity(spawn_queue_capacity);
    let (mailbox, mailbox_rx) = MailboxPlane::new();
    let summarizer: Arc<dyn Summarizer> =
        Arc::new(ModelSummarizer::new(router.clone(), agent.model.clone()));
    let bus = EventBus::new(crate::config::resolve_event_bus_capacity());
    let governor = SubagentGovernor::new(subagent_limits);
    // Clone the router before it is moved into the engine so the team supervisor
    // can test category-candidate servability against the same live providers.
    let spawn_router = router.clone();
    let categories = Arc::new(crate::config::load_categories());
    let sidecar_environment = Arc::new(BundleSidecarEnvironment::production());
    let mut engine_builder = SessionEngine::new(store, router, runtime, permission, bus)
        .with_catalog_refresh(catalog_refresh)
        .with_sidecar_environment(sidecar_environment.clone())
        // Route `categories:` failover chains into the engine's cross-model
        // plane so turn-time pre-stream failures advance through the ordered
        // candidates instead of failing the whole turn.
        .with_model_fallbacks(category_model_fallbacks(&categories))
        .with_compaction(summarizer, compaction_config())
        .with_formatter(formatter_config::load_plane())
        .with_websearch(WebSearchPlane::configured(websearch))
        .with_interaction(interaction)
        // INVARIANT (spawn-intake liveness): the engine *owns* this sender for its
        // whole life -- `SessionEngine::spawner` is a plain field, only ever set by
        // this builder, never taken back out -- and the team supervisor spawned
        // below holds an `Arc<SessionEngine>` for as long as it runs. Therefore the
        // supervisor's `rx.recv()` can never observe a closed intake: the last
        // sender cannot drop while a receiver-owning task is still alive.
        //
        // That invariant is load-bearing. The supervisor's drain branch (see
        // `spawn_team_supervisor_with_environment`) waits for in-flight foreground
        // handlers without watching `stop_child`, which is only safe because a
        // `None` from `rx.recv()` is unreachable in production; today it is reached
        // only from the `spawn_team_supervisor` test helper, which hands the
        // receiver to a supervisor that does not own the sender. A handler stuck in
        // `fail_after_claim`'s `std::future::pending::<()>()` would otherwise make
        // `shutdown()` hang forever instead of aborting.
        //
        // If this ownership ever changes -- engine holding a `Weak`, the sender
        // moving out of the engine, or the supervisor stopping holding the engine
        // -- the drain branch must be made stop-aware in the same change.
        .with_spawn_sender(spawn_sender)
        // INVARIANT (workflow-intake liveness): mirrors `with_spawn_sender` --
        // the engine owns this sender and the workflow worker owns an
        // `Arc<SessionEngine>`, so queued requests always find a live executor.
        .with_workflow_sender(workflow_sender)
        .with_mailbox(mailbox)
        .with_governor(governor);
    if !plugin_host.is_empty() {
        engine_builder = engine_builder.with_hooks(plugin_host.clone());
    }
    let engine = Arc::new(engine_builder);
    let reconciler = Arc::new(RuntimeReconciler::new(engine.runtime_registry()));
    let mcp_control = Arc::new(RuntimeMcpControl::new(reconciler.clone()));
    let plugin_desired = plugin_specs
        .into_iter()
        .map(|spec| DesiredSource::plugin(SourceId::plugin(spec.id.clone()), spec))
        .collect::<Vec<_>>();
    let mcp_desired = mcp
        .iter()
        .map(|(name, config)| DesiredSource::mcp(SourceId::mcp(name.clone()), config.clone()))
        .collect::<Vec<_>>();

    if defer_mcp {
        let plugin_plan = reconciler
            .replace_desired(plugin_desired.clone())
            .context("plan startup plugin reconciliation")?;
        let plugin_results =
            prepared_plugin_results(plugin_plan.sources(), &prepared_plugins, &plugin_failures);
        if let Err(error) = reconciler.finish_revision(&plugin_plan, plugin_results) {
            eprintln!("hya: plugin tool reconciliation rejected ({error})");
        }

        let mut desired = plugin_desired;
        desired.extend(mcp_desired);
        let deferred_plan = reconciler
            .replace_desired(desired)
            .context("plan deferred MCP reconciliation")?;
        let control_bg = mcp_control.clone();
        tokio::spawn(async move {
            if let Err(error) = control_bg.reconcile_plan(deferred_plan).await {
                eprintln!("hya: MCP runtime refresh rejected ({error})");
            }
        });
    } else {
        let mut desired = plugin_desired;
        desired.extend(mcp_desired);
        let plan = reconciler
            .replace_desired(desired)
            .context("plan startup runtime reconciliation")?;
        let mut results =
            prepared_plugin_results(plan.sources(), &prepared_plugins, &plugin_failures);
        results.extend(
            prepare_mcp_results(plan.sources())
                .await
                .context("prepare startup MCP reconciliation")?,
        );
        if let Err(error) = reconciler.finish_revision(&plan, results) {
            eprintln!("hya: startup runtime reconciliation rejected ({error})");
        }
    }
    // Drive resident (long-lived actor) subagents + quiescence (ADR-0002). Started
    // before the team supervisor so its bus subscription is live for the first mail.
    let resident_supervisor = ResidentSupervisor::start_with_owner(engine.clone(), owner_run_id);
    for recovered in recovered_claims {
        let actor_id = recovered.claim.actor_id;
        let (root, _) = engine
            .session_lineage(actor_id)
            .await
            .context("resolve recovered resident root")?;
        let root_projection = engine
            .read_projection(root)
            .await
            .context("replay recovered resident roster")?;
        let entry = root_projection
            .team
            .roster
            .values()
            .find(|entry| entry.session == actor_id && entry.mode == SubagentMode::Resident)
            .cloned()
            .context("active resident claim has no durable roster entry")?;
        let report = engine
            .recover_resident_actor(&recovered, root, &entry.handle)
            .await
            .context("terminalize recovered resident work")?;
        let actor_projection = engine
            .read_projection(actor_id)
            .await
            .context("replay recovered resident session")?;
        let workdir = actor_projection
            .session
            .workdir
            .map(PathBuf::from)
            .unwrap_or_else(|| agent.workdir.clone());
        // Resume is not a new spawn: exact catalog lookup only (no can_spawn,
        // no legacy general/base synthesis). Missing or inline-only identity fails.
        let recorded = actor_projection
            .session
            .agent
            .as_ref()
            .unwrap_or(&entry.agent_type);
        let (recovered_binding, recovered_agent) =
            resolve_recovered_resident_agent(&engine, agent, recorded, &workdir)
                .await
                .with_context(|| {
                    format!(
                        "resolve recovered resident agent `{}` from current catalog",
                        recorded.as_str()
                    )
                })?;
        let recovered_agents = engine
            .agent_roster_for_binding(&recovered_binding, recorded.as_str())
            .with_context(|| {
                format!(
                    "resolve recovered resident roster `{}` from current catalog",
                    recorded.as_str()
                )
            })?;
        let recovered_resources = engine
            .agent_resource_policy_for_binding(&recovered_binding, recorded.as_str())
            .with_context(|| {
                format!(
                    "resolve recovered resident resources `{}` from current catalog",
                    recorded.as_str()
                )
            })?;
        let recovered_sidecar_factory = sidecar_environment
            .factory_for(&recovered_binding, recorded.as_str())
            .with_context(|| {
                format!(
                    "resolve recovered resident sidecar `{}` from current catalog",
                    recorded.as_str()
                )
            })?;
        resident_supervisor
            .register_recovered_resident(
                root,
                entry.handle,
                recovered_agent,
                (
                    recovered_binding,
                    recovered_agents,
                    recovered_resources,
                    recovered_sidecar_factory,
                ),
                recovered,
                report.work,
            )
            .await
            .context("recreate recovered resident runtime owner")?;
    }
    let mut lifecycle = spawn_team_supervisor_with_environment(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        spawn_router,
        categories,
        resident_supervisor.clone(),
        sidecar_environment,
    );
    // Serve `workflow` tool requests from a dedicated worker loop sharing the
    // team supervisor's stop token; runs go through the governed core executor.
    lifecycle.extra_joins.push(spawn_workflow_supervisor(
        workflow_rx,
        engine.clone(),
        agent.clone(),
        resident_supervisor.clone(),
        lifecycle.stop.clone(),
    ));
    // Drive the event-sourced mailbox: append MailSent/Channel*/AgentRegistered to
    // the team-root log and serve roster/channel reads (ADR-0001).
    tokio::spawn(run_mailbox_service(engine.clone(), mailbox_rx));
    Ok(BuiltSessionEngine {
        engine,
        resident_supervisor,
        asks: Some(asks),
        questions: Some(questions),
        mcp_control,
        plugin_host,
        lifecycle,
    })
}

/// Exact-resolve a process-loss recovered resident from the current RuntimeSnapshot.
///
/// Binds once from the recorded session workdir and uses the same production
/// TurnBinding catalog projection as live turns. Resume is definition resolution,
/// not a new spawn: no `can_spawn`, no AgentSpec synthesis, no general/base fallback.
async fn resolve_recovered_resident_agent(
    engine: &SessionEngine,
    base: &AgentSpec,
    recorded_agent: &AgentName,
    session_workdir: &Path,
) -> Result<(TurnBinding, AgentSpec), CoreError> {
    let binding = engine.bind_root_runtime(session_workdir).await?;
    let agent = engine.agent_spec_for_binding(&binding, base, recorded_agent.as_str())?;
    Ok((binding, agent))
}

fn prepared_plugin_results(
    desired: &[DesiredSource],
    prepared: &BTreeMap<String, hya_plugin::PreparedPlugin>,
    failures: &BTreeMap<String, hya_plugin::PluginError>,
) -> Vec<PreparedResult> {
    desired
        .iter()
        .filter(|source| source.id().kind() == hya_core::RuntimeSourceKind::Plugin)
        .map(|source| {
            let id = source.id().configured_id();
            if let Some(plugin) = prepared.get(id) {
                PreparedResult::from(prepared_plugin_source(plugin.clone()))
            } else {
                let error = failures.get(id).map_or_else(
                    || "PLUGIN_START_FAILED: no observed result".to_string(),
                    |error| format!("PLUGIN_START_FAILED: {error}"),
                );
                PreparedResult::from(PreparedFailure::new(source.id().clone(), error))
            }
        })
        .collect()
}

async fn prepare_mcp_results(
    desired: &[DesiredSource],
) -> Result<Vec<PreparedResult>, crate::runtime_reconcile::ReconcileError> {
    let mut set = tokio::task::JoinSet::new();
    let mut tasks = BTreeMap::new();
    for source in desired
        .iter()
        .filter(|source| source.id().kind() == hya_core::RuntimeSourceKind::Mcp)
        .cloned()
    {
        let id = source.id().clone();
        let handle = set.spawn(prepare_desired_source(source));
        tasks.insert(handle.id(), id);
    }
    let mut results = Vec::new();
    while let Some(joined) = set.join_next_with_id().await {
        match joined {
            Ok((id, result)) => {
                tasks.remove(&id);
                results.push(result);
            }
            Err(error) => {
                let Some(source) = tasks.remove(&error.id()) else {
                    return Err(crate::runtime_reconcile::ReconcileError::InvalidPrepared(
                        format!("MCP preparation task {} had no source ticket", error.id()),
                    ));
                };
                results.push(PreparedResult::from(PreparedFailure::new(
                    source,
                    format!("MCP_PREPARE_TASK_FAILED: {error}"),
                )));
            }
        }
    }
    Ok(results)
}

/// Inputs for [`HyaRuntime::start`] (store path, model, and safety flags).
pub struct RuntimeOptions {
    /// Override default model; `None` uses config / offline default.
    pub model: Option<String>,
    /// SQLite path, or empty string for an in-memory store.
    pub db: String,
    /// When true, auto-approve every tool action (Danger permission model).
    pub yolo: bool,
    /// Override the preferred primary agent id for new sessions.
    pub default_agent: Option<String>,
    /// When true, skip live config and always use the offline dev provider.
    pub force_offline: bool,
}

/// Process entry point: session engine + axum router ready to serve or embed.
///
/// Holds the plugin host and built engine for the process lifetime so side
/// planes stay connected until the runtime is dropped.
pub struct HyaRuntime {
    router: axum::Router,
    engine: Arc<SessionEngine>,
    app_state: hya_server::AppState,
    _plugin_host: Arc<hya_plugin::PluginHost>,
    _built: BuiltSessionEngine,
}

impl HyaRuntime {
    /// Open the store, resolve providers, build the engine, and install the HTTP router.
    ///
    /// When `force_offline` is set, uses only [`offline_router`]. Otherwise loads
    /// config via [`resolve_runtime`]. `yolo` forces Danger permissions and logs
    /// a stderr warning. Returns a runtime whose [`HyaRuntime::router`] can be
    /// served with axum (or inspected by embedders).
    pub async fn start(opts: RuntimeOptions) -> anyhow::Result<Self> {
        let store = open_store(&opts.db).await?;
        let runtime = if opts.force_offline {
            let (router, model) = offline_router(opts.model);
            RuntimeConfig {
                router,
                model,
                reasoning: None,
                models: Vec::new(),
                mcp: BTreeMap::new(),
                plugins: Vec::new(),
                default_agent: opts.default_agent,
                categories: CategoryRegistry::default(),
                offline_notice: None,
                permission: InvocationPolicy::default(),
                websearch: WebSearchConfig::default(),
            }
        } else {
            let mut runtime = resolve_runtime(opts.model);
            if opts.default_agent.is_some() {
                runtime.default_agent = opts.default_agent;
            }
            runtime
        }
        .with_yolo(opts.yolo);
        if opts.yolo {
            eprintln!("hya: --yolo auto-approves ALL tool actions for the hya frontend (RCE risk)");
        }
        // Server/TUI AppState: agent base only. Per-turn guidance layers
        // Environment + current workdir AGENTS + references once.
        let agent = Arc::new(agent_base_with_model(&runtime.model, runtime.reasoning));
        let mut built = build_session_engine(
            store,
            runtime.router,
            agent.as_ref(),
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?;
        let engine = built.engine();
        let questions = built.take_questions().ok_or_else(|| {
            anyhow::anyhow!("BuiltSessionEngine questions receiver already taken")
        })?;
        let asks = built
            .take_asks()
            .ok_or_else(|| anyhow::anyhow!("BuiltSessionEngine asks receiver already taken"))?;
        let mcp_control = built.mcp_control();
        let plugin_host = built.plugin_host();
        let mut state = hya_server::AppState::new(engine.clone(), agent)
            .with_question_requests(questions)
            .with_mcp_control(mcp_control)
            .with_workspace_adapters(plugin_host.workspace_adapters())
            .with_default_agent(runtime.default_agent.clone());
        state = state.with_permission_requests(asks);
        let app_state = state.clone();
        let router = hya_server::router(state);
        Ok(Self {
            router,
            engine,
            app_state,
            _plugin_host: plugin_host,
            _built: built,
        })
    }

    /// Axum router with native and Compat routes (from [`hya_server::router`]).
    pub fn router(&self) -> &axum::Router {
        &self.router
    }

    /// Shared session engine for in-process callers (same instance the router uses).
    #[must_use]
    pub fn engine(&self) -> Arc<SessionEngine> {
        self.engine.clone()
    }

    /// Clone of the HTTP `AppState` wrapped by the router.
    #[must_use]
    pub fn app_state(&self) -> hya_server::AppState {
        self.app_state.clone()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    /// Test shim: wrap an installed-bundle catalog as an [`AgentCatalog`] over
    /// the compiled-in built-ins.
    fn to_agent_catalog(bundles: BundleCatalog) -> AgentCatalog {
        AgentCatalog::new(Arc::new(bundles)).expect("valid agent catalog")
    }

    use super::*;
    use async_trait::async_trait;
    use hya_bundle::{
        AgentRole, BundleIdentity, BundleSource, ModelPolicy, PreparedAgent, PreparedBundle,
        PreparedResource, ResourceView, SourceFile, prepare_package,
    };
    use hya_core::{CategoryEntry, run_team};
    use hya_plugin::messages::{METHOD_TOOL_CALL, ToolCallParams, ToolInfo};
    use hya_plugin::protocol::Frame;
    use hya_proto::{
        Event, FinishReason, MailEndpoint, MailKind, MemberRunStatus, OwnerRunId, RosterStatus,
        SubagentMode, ToolName, ToolSchema,
    };
    use hya_provider::{
        Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, HttpProvider,
        Provider, ProviderError, ProviderKind,
    };
    use hya_store::{BundleInstallCandidate, BundleInstallOutcome, BundleRegistry};
    use hya_tool::{
        AgentDef, FormatterPlane, InlineAgent, InteractionPlane, LspPlane, MailboxPlane, Mode,
        PermissionModel, PermissionPlane, PermissionRules, Rule, SkillPlane, SpawnerPlane,
        TodoPlane, Tool, ToolCtx, ToolError, ToolOperation, ToolPermission, ToolRegistry,
        WebSearchPlane,
    };
    use serde_json::{Value, json};
    use sqlx::{Connection, SqliteConnection};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::AsyncBufReadExt;
    use tokio_util::sync::CancellationToken;

    /// Guards the process-global environment that `EnvGuard` mutates.
    ///
    /// A `RwLock`, not a `Mutex`, because two different populations need it:
    ///
    /// - **Writers** (`EnvGuard::set`) repoint `HOME`, `XDG_*` and the process
    ///   current directory while they run.
    /// - **Readers** (`StableEnvGuard`) do not touch the environment, but their
    ///   assertions depend on it holding still.
    ///
    /// The second population is not obvious, so it is worth stating why it
    /// exists. `hya_tool::skill_dirs_for_workdir` builds the skill search path
    /// from `HOME` (`crates/hya-tool/src/skill_catalog.rs:46-61`), the skill set
    /// feeds `TurnBinding::semantic_fingerprint_v1`, and durable spawn admission
    /// compares the fingerprint recorded in an intent against one recomputed
    /// later. If `HOME` changes in between, the two disagree and resolution
    /// fails closed as `SpawnError::Unavailable` — surfacing only as a resolved
    /// launch count of 0. Any test that spans a fingerprint capture and a
    /// fingerprint recomputation therefore needs `HOME` pinned for its duration.
    ///
    /// Readers still run concurrently with each other, so this costs
    /// parallelism only against the handful of writers.
    static ENV_LOCK: std::sync::RwLock<()> = std::sync::RwLock::new(());

    /// Read-side companion to [`EnvGuard`]: pins the process environment for the
    /// lifetime of a test that must observe a stable `HOME`-derived runtime
    /// fingerprint, without mutating anything itself.
    ///
    /// Poisoning is ignored deliberately — a panic in one environment test must
    /// not cascade into spurious failures in every other test that takes this
    /// guard. Same rationale as the `hya-sdk` `ENV_GUARD` added in `0acfc919`.
    struct StableEnvGuard {
        _lock: std::sync::RwLockReadGuard<'static, ()>,
    }

    impl StableEnvGuard {
        fn acquire() -> Self {
            Self {
                _lock: ENV_LOCK
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            }
        }
    }

    struct CountingDevProvider {
        calls: Arc<AtomicUsize>,
        inner: DevProvider,
        gate: Option<Arc<ProviderGate>>,
    }

    struct ProviderGate {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    #[async_trait]
    impl Provider for CountingDevProvider {
        fn id(&self) -> &str {
            self.inner.id()
        }

        fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
            self.inner.capabilities(model)
        }

        fn configured_identity_v1(&self) -> Option<Vec<u8>> {
            self.inner.configured_identity_v1()
        }

        async fn stream(
            &self,
            request: CompletionRequest,
            session: SessionId,
            message: hya_proto::MessageId,
        ) -> Result<EventStream, ProviderError> {
            if let Some(gate) = &self.gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.inner.stream(request, session, message).await
        }
    }

    #[test]
    fn foreground_admission_wake_router_bounds_and_coalesces() {
        let observer = Arc::new(AdmissionTestObserver::new());
        let router = Arc::new(ForegroundAdmissionWakeRouter::new(Some(Arc::clone(
            &observer,
        ))));
        let operation_id =
            ToolOperation::from_tool_call(hya_proto::ToolCallId::new()).operation_id();
        let (first, mut first_rx) = router.register(operation_id).expect("first route");
        let generation = first.generation;
        router.wake(operation_id);
        router.wake(operation_id);
        assert_eq!(
            observer.foreign_wake_operation_ids.lock().unwrap().len(),
            1,
            "capacity-one duplicate wakes must coalesce without retry"
        );
        assert_eq!(
            first_rx.try_recv().ok().map(|wake| wake._generation),
            Some(generation)
        );
        assert_eq!(first_rx.try_recv().ok().map(|wake| wake._generation), None);
        router.wake(operation_id);
        assert_eq!(
            observer.foreign_wake_operation_ids.lock().unwrap().len(),
            2,
            "a wake after the coalesced payload is consumed may send once"
        );

        let mut registrations = vec![first];
        for _ in 1..FOREGROUND_HANDLER_CAP {
            let operation_id =
                ToolOperation::from_tool_call(hya_proto::ToolCallId::new()).operation_id();
            registrations.push(router.register(operation_id).expect("route within cap").0);
        }
        let overflow = ToolOperation::from_tool_call(hya_proto::ToolCallId::new()).operation_id();
        assert!(
            router.register(overflow).is_none(),
            "wake index must reject registration beyond the foreground cap"
        );
    }

    #[test]
    fn foreground_admission_wake_router_rejects_stale_guard_generation() {
        let observer = Arc::new(AdmissionTestObserver::new());
        let router = Arc::new(ForegroundAdmissionWakeRouter::new(Some(Arc::clone(
            &observer,
        ))));
        let operation_id =
            ToolOperation::from_tool_call(hya_proto::ToolCallId::new()).operation_id();
        let (stale, _stale_rx) = router.register(operation_id).expect("stale route");
        let replacement_generation = stale.generation.wrapping_add(1);
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        router.routes.lock().unwrap().insert(
            operation_id,
            ForegroundAdmissionWakeRoute {
                generation: replacement_generation,
                sender,
            },
        );
        drop(stale);
        assert_eq!(
            router
                .routes
                .lock()
                .unwrap()
                .get(&operation_id)
                .map(|route| route.generation),
            Some(replacement_generation),
            "stale guard drop must not remove a newer registration"
        );
        router.wake(operation_id);
        assert_eq!(
            receiver.try_recv().ok().map(|wake| wake._generation),
            Some(replacement_generation),
            "wake must target the newer registration generation"
        );
        assert_eq!(
            observer
                .foreign_wake_operation_ids
                .lock()
                .unwrap()
                .as_slice(),
            &[operation_id]
        );
    }

    #[test]
    fn resident_owner_run_id_is_stable_for_the_process() {
        assert_eq!(process_owner_run_id(), process_owner_run_id());
    }

    #[test]
    fn builtin_agent_catalog_starts_with_no_installed_bundles() {
        let catalog = builtin_agent_catalog().expect("builtin agent catalog must build");
        assert!(
            catalog.bundles().bundles().is_empty(),
            "a fresh process has no installed bundles"
        );
        for id in ["build", "plan", "explore", "general", "hya-main"] {
            assert!(
                catalog.resolve(id).is_some(),
                "builtin `{id}` must resolve without any installed bundle"
            );
        }
    }

    #[test]
    fn builtin_agent_catalog_retains_semantic_identity_when_empty() {
        // The fingerprint must stay available on a fresh install; an empty
        // installed catalog is "nothing to attest", not "unidentifiable".
        let catalog = builtin_agent_catalog().expect("builtin agent catalog must build");
        assert!(
            catalog
                .semantic_identity_v1()
                .is_some_and(|identity| !identity.is_empty()),
            "empty installed catalog must still yield a semantic identity"
        );
    }

    #[test]
    fn admission_binding_fingerprint_is_stable_for_fresh_equivalent_contexts() {
        let make_context = || {
            AdmissionResolutionContext::capture(
                AgentSpec {
                    name: AgentName::new("build"),
                    model: ModelRef::new("fixture/model"),
                    system_prompt: "captured harness base".to_string(),
                    workdir: PathBuf::from("fixture-workdir"),
                    reasoning: Some(ReasoningEffort::High),
                },
                Arc::new(CategoryRegistry::default()),
                Arc::new(ProviderRouter::new()),
            )
            .expect("an empty configured router has a deterministic semantic identity")
        };
        let runtime_fingerprint = [0x5a; 32];

        let first = make_context().admission_binding_fingerprint_v1(runtime_fingerprint);
        let reconstructed = make_context().admission_binding_fingerprint_v1(runtime_fingerprint);

        assert_eq!(first, reconstructed);
        assert_ne!(first, [0; 32]);
    }

    #[tokio::test]
    async fn admission_binding_base_fields_match_reconstructed_agent() {
        let binding_workdir = tempdir();
        let engine = engine_with_catalog(catalog_with_worker_policy(ModelPolicy::default())).await;
        let binding = engine
            .bind_runtime(&binding_workdir)
            .expect("worker binding must be available");
        let runtime_fingerprint = [0x5a; 32];
        let make_base = |name: &str,
                         model: &str,
                         system_prompt: &str,
                         workdir: &Path,
                         reasoning: Option<ReasoningEffort>| AgentSpec {
            name: AgentName::new(name),
            model: ModelRef::new(model),
            system_prompt: system_prompt.to_string(),
            workdir: workdir.to_path_buf(),
            reasoning,
        };
        let resolve = |base: AgentSpec| {
            let expected = engine
                .agent_spec_for_binding(&binding, &base, "worker")
                .expect("worker definition must resolve");
            let context = AdmissionResolutionContext::capture(
                base,
                Arc::new(CategoryRegistry::default()),
                Arc::new(ProviderRouter::new()),
            )
            .expect("empty category/provider context must capture");
            let fingerprint = context.admission_binding_fingerprint_v1(runtime_fingerprint);
            let reconstructed = context
                .resolve_agent_for_binding(&engine, &binding, "worker")
                .expect("captured worker context must resolve");
            (fingerprint, expected, reconstructed)
        };

        let baseline_base = make_base(
            "build",
            "fixture/base",
            "base prompt",
            Path::new("/tmp/base-workdir"),
            Some(ReasoningEffort::High),
        );
        let model_base = make_base(
            "build",
            "fixture/model-change",
            "base prompt",
            Path::new("/tmp/base-workdir"),
            Some(ReasoningEffort::High),
        );
        let prompt_base = make_base(
            "build",
            "fixture/base",
            "prompt change",
            Path::new("/tmp/base-workdir"),
            Some(ReasoningEffort::High),
        );
        let reasoning_base = make_base(
            "build",
            "fixture/base",
            "base prompt",
            Path::new("/tmp/base-workdir"),
            Some(ReasoningEffort::Low),
        );
        let overwritten_base = make_base(
            "renamed",
            "fixture/base",
            "base prompt",
            Path::new("/tmp/overwritten-workdir"),
            Some(ReasoningEffort::High),
        );

        let (baseline_fingerprint, baseline_expected, baseline) = resolve(baseline_base.clone());
        let (model_fingerprint, model_expected, model) = resolve(model_base.clone());
        let (prompt_fingerprint, prompt_expected, prompt) = resolve(prompt_base.clone());
        let (reasoning_fingerprint, reasoning_expected, reasoning) =
            resolve(reasoning_base.clone());
        let (overwritten_fingerprint, overwritten_expected, overwritten) =
            resolve(overwritten_base.clone());

        assert_ne!(baseline_fingerprint, model_fingerprint);
        assert_ne!(baseline_fingerprint, prompt_fingerprint);
        assert_ne!(baseline_fingerprint, reasoning_fingerprint);
        assert_eq!(baseline.model, baseline_expected.model);
        assert_eq!(baseline.system_prompt, baseline_expected.system_prompt);
        assert_eq!(baseline.reasoning, baseline_expected.reasoning);
        assert_eq!(model.model, model_expected.model);
        assert_eq!(model.system_prompt, model_expected.system_prompt);
        assert_eq!(model.reasoning, model_expected.reasoning);
        assert_eq!(prompt.model, prompt_expected.model);
        assert_eq!(prompt.system_prompt, prompt_expected.system_prompt);
        assert_eq!(prompt.reasoning, prompt_expected.reasoning);
        assert_eq!(reasoning.model, reasoning_expected.model);
        assert_eq!(reasoning.system_prompt, reasoning_expected.system_prompt);
        assert_eq!(reasoning.reasoning, reasoning_expected.reasoning);
        assert_eq!(overwritten_fingerprint, baseline_fingerprint);
        assert_eq!(overwritten.name.as_str(), "worker");
        assert_eq!(overwritten.workdir, binding.workdir());
        assert_eq!(overwritten.model, overwritten_expected.model);
        assert_eq!(
            overwritten.system_prompt,
            overwritten_expected.system_prompt
        );
        assert_eq!(overwritten.reasoning, overwritten_expected.reasoning);
    }

    #[test]
    fn admission_binding_category_fields_match_resolution_semantics() {
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fixture/base"),
            system_prompt: "captured harness base".to_string(),
            workdir: PathBuf::from("fixture-workdir"),
            reasoning: Some(ReasoningEffort::High),
        };
        let entry =
            |model: &str, fallback: &[&str], prompt_append: &str, token_budget: Option<u64>| {
                CategoryEntry {
                    model: ModelRef::new(model),
                    fallback: fallback
                        .iter()
                        .map(|candidate| ModelRef::new(*candidate))
                        .collect(),
                    prompt_append: prompt_append.to_string(),
                    token_budget,
                }
            };
        let categories = |primary_key: &str, primary: CategoryEntry, secondary: CategoryEntry| {
            let mut entries = HashMap::new();
            entries.insert(primary_key.to_string(), primary);
            entries.insert("secondary".to_string(), secondary);
            CategoryRegistry::from_entries(entries)
        };
        let capture = |categories: CategoryRegistry| {
            AdmissionResolutionContext::capture(
                base.clone(),
                Arc::new(categories),
                Arc::new(ProviderRouter::new()),
            )
            .expect("category context must capture")
        };
        let select = |context: &AdmissionResolutionContext, category: &str| {
            context.resolve_category_for_admission(category)
        };
        let runtime_fingerprint = [0x5a; 32];

        let baseline = capture(categories(
            "primary",
            entry("provider/first", &["provider/second"], "", None),
            entry("provider/secondary", &[], "", None),
        ));
        let reversed = {
            let mut entries = HashMap::new();
            entries.insert(
                "secondary".to_string(),
                entry("provider/secondary", &[], "", None),
            );
            entries.insert(
                "primary".to_string(),
                entry("provider/first", &["provider/second"], "", None),
            );
            capture(CategoryRegistry::from_entries(entries))
        };
        let swapped = capture(categories(
            "primary",
            entry("provider/second", &["provider/first"], "", None),
            entry("provider/secondary", &[], "", None),
        ));
        let renamed = capture(categories(
            "renamed",
            entry("provider/first", &["provider/second"], "", None),
            entry("provider/secondary", &[], "", None),
        ));
        let shaping_only = capture(categories(
            "primary",
            entry(
                "provider/first",
                &["provider/second"],
                "unused prompt",
                Some(42),
            ),
            entry("provider/secondary", &[], "unused secondary", Some(7)),
        ));

        let baseline_fingerprint = baseline.admission_binding_fingerprint_v1(runtime_fingerprint);
        assert_eq!(
            baseline_fingerprint,
            reversed.admission_binding_fingerprint_v1(runtime_fingerprint)
        );
        assert_eq!(
            select(&baseline, "primary"),
            Some(ModelRef::new("provider/first"))
        );
        assert_eq!(
            select(&baseline, "secondary"),
            Some(ModelRef::new("provider/secondary"))
        );
        assert_eq!(select(&reversed, "primary"), select(&baseline, "primary"));
        assert_eq!(
            select(&reversed, "secondary"),
            select(&baseline, "secondary")
        );
        assert_ne!(
            baseline_fingerprint,
            swapped.admission_binding_fingerprint_v1(runtime_fingerprint)
        );
        assert_eq!(
            select(&swapped, "primary"),
            Some(ModelRef::new("provider/second"))
        );
        assert_ne!(
            baseline_fingerprint,
            renamed.admission_binding_fingerprint_v1(runtime_fingerprint)
        );
        assert_eq!(select(&renamed, "primary"), None);
        assert_eq!(
            select(&renamed, "renamed"),
            Some(ModelRef::new("provider/first"))
        );
        assert_eq!(
            baseline_fingerprint,
            shaping_only.admission_binding_fingerprint_v1(runtime_fingerprint)
        );
        assert_eq!(
            select(&shaping_only, "primary"),
            select(&baseline, "primary")
        );
        assert_eq!(
            select(&shaping_only, "secondary"),
            select(&baseline, "secondary")
        );
    }

    #[test]
    fn admission_binding_provider_fields_match_resolution_semantics() {
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fixture/base"),
            system_prompt: "captured harness base".to_string(),
            workdir: PathBuf::from("fixture-workdir"),
            reasoning: Some(ReasoningEffort::High),
        };
        let mut entries = HashMap::new();
        entries.insert(
            "quality".to_string(),
            CategoryEntry {
                model: ModelRef::new("route/primary"),
                fallback: vec![ModelRef::new("route/fallback")],
                prompt_append: String::new(),
                token_budget: None,
            },
        );
        let categories = Arc::new(CategoryRegistry::from_entries(entries));

        let first_resolver_called = Arc::new(AtomicBool::new(false));
        let first_resolver_flag = Arc::clone(&first_resolver_called);
        let first_provider = HttpProvider::new(
            "route",
            ProviderKind::OpenAiCompatible,
            "https://route.example/v1/",
            "credential-a".to_string(),
            ["primary".to_string(), "fallback".to_string()],
        )
        .expect("first HTTP route must construct")
        .with_bearer_resolver(Arc::new(move || {
            first_resolver_flag.store(true, Ordering::SeqCst);
            Ok("live-token-a".to_string())
        }));
        let first_router = Arc::new(ProviderRouter::new().with(Arc::new(first_provider)));

        let first_context = AdmissionResolutionContext::capture(
            base.clone(),
            Arc::clone(&categories),
            Arc::clone(&first_router),
        )
        .expect("configured provider router must capture");
        let runtime_fingerprint = [0x5a; 32];
        let first_fingerprint = first_context.admission_binding_fingerprint_v1(runtime_fingerprint);

        let second_resolver_called = Arc::new(AtomicBool::new(false));
        let second_resolver_flag = Arc::clone(&second_resolver_called);
        let second_provider = HttpProvider::new(
            "route",
            ProviderKind::OpenAiCompatible,
            "https://route.example/v1",
            "credential-b".to_string(),
            ["primary".to_string(), "fallback".to_string()],
        )
        .expect("second HTTP route must construct")
        .with_bearer_resolver(Arc::new(move || {
            second_resolver_flag.store(true, Ordering::SeqCst);
            Ok("live-token-b".to_string())
        }));
        let second_router = Arc::new(ProviderRouter::new().with(Arc::new(second_provider)));
        let second_context = AdmissionResolutionContext::capture(
            base.clone(),
            Arc::clone(&categories),
            Arc::clone(&second_router),
        )
        .expect("equivalent configured provider router must capture");
        let second_fingerprint =
            second_context.admission_binding_fingerprint_v1(runtime_fingerprint);

        assert_eq!(first_fingerprint, second_fingerprint);
        assert!(!first_resolver_called.load(Ordering::SeqCst));
        assert!(!second_resolver_called.load(Ordering::SeqCst));
        assert_eq!(
            first_context.resolve_category_for_admission("quality"),
            Some(ModelRef::new("route/primary"))
        );
        assert_eq!(
            second_context.resolve_category_for_admission("quality"),
            Some(ModelRef::new("route/primary"))
        );
        assert!(!first_resolver_called.load(Ordering::SeqCst));
        assert!(!second_resolver_called.load(Ordering::SeqCst));

        let endpoint_provider = HttpProvider::new(
            "route",
            ProviderKind::OpenAiCompatible,
            "https://route.example/v2",
            "credential-a".to_string(),
            ["primary".to_string(), "fallback".to_string()],
        )
        .expect("changed-endpoint HTTP route must construct");
        let endpoint_context = AdmissionResolutionContext::capture(
            base.clone(),
            Arc::clone(&categories),
            Arc::new(ProviderRouter::new().with(Arc::new(endpoint_provider))),
        )
        .expect("changed-endpoint provider router must capture");
        assert_ne!(
            first_fingerprint,
            endpoint_context.admission_binding_fingerprint_v1(runtime_fingerprint)
        );

        let fallback_provider = HttpProvider::new(
            "route",
            ProviderKind::OpenAiCompatible,
            "https://route.example/v1",
            "credential-a".to_string(),
            ["fallback".to_string()],
        )
        .expect("fallback-only HTTP route must construct");
        let fallback_context = AdmissionResolutionContext::capture(
            base.clone(),
            Arc::clone(&categories),
            Arc::new(ProviderRouter::new().with(Arc::new(fallback_provider))),
        )
        .expect("fallback-only provider router must capture");
        assert_ne!(
            first_fingerprint,
            fallback_context.admission_binding_fingerprint_v1(runtime_fingerprint)
        );
        assert_eq!(
            fallback_context.resolve_category_for_admission("quality"),
            Some(ModelRef::new("route/fallback"))
        );

        let route_a = || {
            HttpProvider::new(
                "route-a",
                ProviderKind::OpenAiCompatible,
                "https://route.example/shared",
                "credential".to_string(),
                ["primary".to_string()],
            )
            .expect("route-a HTTP provider must construct")
        };
        let route_b = || {
            HttpProvider::new(
                "route-b",
                ProviderKind::OpenAiCompatible,
                "https://route.example/shared",
                "credential".to_string(),
                ["primary".to_string()],
            )
            .expect("route-b HTTP provider must construct")
        };
        let router_ab = Arc::new(
            ProviderRouter::new()
                .with(Arc::new(route_a()))
                .with(Arc::new(route_b())),
        );
        let router_ba = Arc::new(
            ProviderRouter::new()
                .with(Arc::new(route_b()))
                .with(Arc::new(route_a())),
        );
        let context_ab =
            AdmissionResolutionContext::capture(base.clone(), Arc::clone(&categories), router_ab)
                .expect("route-a then route-b provider router must capture");
        let context_ba = AdmissionResolutionContext::capture(base.clone(), categories, router_ba)
            .expect("route-b then route-a provider router must capture");
        assert_ne!(
            context_ab.admission_binding_fingerprint_v1(runtime_fingerprint),
            context_ba.admission_binding_fingerprint_v1(runtime_fingerprint)
        );

        let fake_router =
            Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(Vec::new()))));
        let fake_error = match AdmissionResolutionContext::capture(
            base,
            Arc::new(CategoryRegistry::default()),
            fake_router,
        ) {
            Ok(_) => panic!("a provider without configured identity must fail closed"),
            Err(error) => error,
        };
        assert_eq!(format!("{fake_error:?}"), "ProviderIdentityUnavailable");
    }

    #[tokio::test]
    async fn spawn_admission_prepares_canonical_intents_before_runtime_resolution() {
        // Pins `HOME` for this test: it captures a runtime fingerprint and then
        // asserts the one `prepare_spawn_admission` recomputes is equal to it.
        // See `ENV_LOCK`.
        let _env = StableEnvGuard::acquire();
        let workdir = tempdir().join("spawn-admission-workdir-sentinel");
        std::fs::create_dir_all(&workdir).unwrap();
        let engine =
            engine_with_catalog(builtin_agent_catalog().expect("built-in catalog must load")).await;
        let binding = engine.bind_runtime(&workdir).expect("bind admission turn");

        let base_model = "derived-base-model-sentinel";
        let base_system_prompt = "derived-base-system-prompt-sentinel";
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new(base_model),
            system_prompt: base_system_prompt.to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };

        let categories = Arc::new(CategoryRegistry::default());
        let provider_endpoint = "https://configured-provider-endpoint-sentinel.example/v1/";
        let provider_config = "configured-provider-config-sentinel";
        let bearer_resolver_called = Arc::new(AtomicBool::new(false));
        let resolver_flag = Arc::clone(&bearer_resolver_called);
        let provider = HttpProvider::new(
            "configured-provider-id-sentinel",
            ProviderKind::OpenAiCompatible,
            provider_endpoint,
            provider_config.to_string(),
            ["configured-provider-model-sentinel".to_string()],
        )
        .expect("configured provider must construct")
        .with_bearer_resolver(Arc::new(move || {
            resolver_flag.store(true, Ordering::SeqCst);
            Ok("live-token-sentinel".to_string())
        }));
        let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));

        let agents: Arc<[AgentDef]> = vec![AgentDef {
            name: "general".to_string(),
            description: None,
            category: None,
            mode: "subagent".to_string(),
        }]
        .into();
        let guidance = "request-guidance-sentinel";
        let raw_members: Vec<(String, String)> = (0..101)
            .map(|ordinal| {
                (
                    format!("raw-description-{ordinal}-sentinel"),
                    format!("raw-prompt-{ordinal}-sentinel"),
                )
            })
            .collect();
        let members = raw_members
            .iter()
            .map(|(description, prompt)| SpawnMember {
                description: description.clone(),
                prompt: prompt.clone(),
                subagent_type: String::new(),
                task_id: None,
                model: None,
                category: None,
                inline_agent: None,
                resident: false,
            })
            .collect();
        let (reply, _reply_rx) = tokio::sync::oneshot::channel();
        let request = SpawnRequest {
            parent: SessionId::new(),
            agents,
            guidance: Some(Arc::<str>::from(guidance)),
            operation: ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
            members,
            cancel: CancellationToken::new(),
            background: true,
            reply,
        };

        let expected_request_fingerprint =
            spawn_request_fingerprint(&request).expect("request fingerprint must serialize");
        let expected_runtime_fingerprint = engine
            .runtime_semantic_fingerprint_v1(&binding)
            .expect("built-in runtime binding must have a semantic fingerprint");
        let before = engine
            .store()
            .admission_counts()
            .await
            .expect("read admission counts before preparation");
        assert_eq!(
            before,
            hya_store::AdmissionCounts {
                active: 0,
                non_active: 0,
                total: 0,
            }
        );

        let prepared = prepare_spawn_admission(
            &engine,
            &binding,
            base,
            Arc::clone(&categories),
            Arc::clone(&router),
            "build",
            &request,
        )
        .expect("spawn admission preparation must be pure");

        let after = engine
            .store()
            .admission_counts()
            .await
            .expect("read admission counts after preparation");
        assert_eq!(after, before);
        assert_eq!(prepared.request_fingerprint, expected_request_fingerprint);
        assert!(Arc::ptr_eq(&prepared.resolution.categories, &categories));
        assert!(Arc::ptr_eq(&prepared.resolution.router, &router));
        assert_eq!(prepared.intents.len(), 101);

        let expected_admission_fingerprint = prepared
            .resolution
            .admission_binding_fingerprint_v1(expected_runtime_fingerprint);
        let contains = |bytes: &[u8], needle: &str| {
            bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
        };
        for (ordinal, intent) in prepared.intents.iter().enumerate() {
            assert_eq!(intent.runtime_fingerprint_version, 1);
            assert_eq!(intent.runtime_fingerprint, expected_runtime_fingerprint);
            assert_ne!(intent.runtime_fingerprint, [0; 32]);
            assert_eq!(intent.admission_binding_fingerprint_version, 1);
            assert_eq!(
                intent.admission_binding_fingerprint,
                expected_admission_fingerprint
            );

            let encoded = &intent.spawn_intent;
            assert!(encoded.len() <= hya_store::MAX_ADMISSION_INTENT_BYTES);
            assert!(contains(encoded, &raw_members[ordinal].0));
            assert!(contains(encoded, &raw_members[ordinal].1));
            assert!(contains(encoded, "general"));
            assert!(!contains(encoded, base_model));
            assert!(!contains(encoded, base_system_prompt));
            assert!(!contains(encoded, provider_endpoint));
            assert!(!contains(encoded, provider_config));
            assert!(!contains(encoded, guidance));

            let integrity_width = 32;
            let suffix =
                &encoded[encoded.len() - integrity_width - 9..encoded.len() - integrity_width];
            assert_eq!(
                u32::from_be_bytes(suffix[0..4].try_into().unwrap()),
                ordinal as u32
            );
            assert_eq!(u32::from_be_bytes(suffix[4..8].try_into().unwrap()), 101);
            assert_eq!(suffix[8], 0);
        }
        assert!(
            prepared
                .intents
                .windows(2)
                .all(|pair| pair[0].spawn_intent != pair[1].spawn_intent)
        );
        assert!(!bearer_resolver_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn accepted_admission_launches_resolve_without_touching_queued_members() {
        // Pins `HOME` for this test: it claims an admission batch (recording a
        // runtime fingerprint in each intent) and then resolves those launches,
        // which recomputes the fingerprint and compares. See `ENV_LOCK`.
        let _env = StableEnvGuard::acquire();
        let workdir = tempdir().join("accepted-admission-workdir-sentinel");
        std::fs::create_dir_all(&workdir).unwrap();
        let engine =
            engine_with_catalog(builtin_agent_catalog().expect("built-in catalog must load")).await;
        let binding = engine.bind_runtime(&workdir).expect("bind admission turn");
        let base_model = "accepted-base-model-sentinel";
        let base_system_prompt = "accepted-base-system-prompt-sentinel";
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new(base_model),
            system_prompt: base_system_prompt.to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let categories = Arc::new(CategoryRegistry::default());
        let router = Arc::new(ProviderRouter::new());
        let agents: Arc<[AgentDef]> = vec![AgentDef {
            name: "general".to_string(),
            description: None,
            category: None,
            mode: "subagent".to_string(),
        }]
        .into();
        let raw_members: Vec<(String, String)> = (0..101)
            .map(|ordinal| {
                (
                    format!("accepted-description-{ordinal}-sentinel"),
                    format!("accepted-prompt-{ordinal}-sentinel"),
                )
            })
            .collect();
        let members = raw_members
            .iter()
            .map(|(description, prompt)| SpawnMember {
                description: description.clone(),
                prompt: prompt.clone(),
                subagent_type: "general".to_string(),
                task_id: None,
                model: None,
                category: None,
                inline_agent: None,
                resident: false,
            })
            .collect();
        let parent = SessionId::new();
        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let (reply, _reply_rx) = tokio::sync::oneshot::channel();
        let request = SpawnRequest {
            parent,
            agents,
            guidance: Some(Arc::<str>::from("accepted-guidance-sentinel")),
            operation,
            members,
            cancel: CancellationToken::new(),
            background: true,
            reply,
        };
        let PreparedSpawnAdmission {
            request_fingerprint,
            resolution,
            intents,
        } = prepare_spawn_admission(
            &engine,
            &binding,
            base,
            Arc::clone(&categories),
            Arc::clone(&router),
            "build",
            &request,
        )
        .expect("prepare canonical admission intents");
        let root_session = SessionId::new();
        assert_ne!(root_session, request.parent);
        let claim = hya_store::AdmissionClaim {
            operation_id: request.operation.operation_id(),
            source_tool_call_id: request.operation.source_tool_call_id(),
            root_session,
            request_fingerprint,
            admission_units: 101,
            actor_claim: request.operation.actor_claim(),
        };
        let claim_outcome = engine
            .store()
            .claim_admission_batch(&claim, intents)
            .await
            .expect("claim canonical admission batch");
        let launches = match claim_outcome {
            hya_store::AdmissionBatchClaimOutcome::Claimed(launches) => launches,
            hya_store::AdmissionBatchClaimOutcome::Existing => {
                panic!("fresh store-only fixture must claim the admission batch")
            }
        };
        assert_eq!(launches.len(), 100);
        for (ordinal, launch) in launches.iter().enumerate() {
            assert_eq!(launch.record.member_ordinal, ordinal as u32);
            assert_eq!(launch.record.state, hya_store::AdmissionState::Accepted);
        }
        let counts = engine
            .store()
            .admission_counts()
            .await
            .expect("read admitted counts");
        assert_eq!(
            counts,
            hya_store::AdmissionCounts {
                active: 100,
                non_active: 1,
                total: 101,
            }
        );
        let records = engine
            .store()
            .admissions(request.operation.operation_id())
            .await
            .expect("read admitted rows");
        assert_eq!(records.len(), 101);
        for (ordinal, record) in records.iter().enumerate() {
            assert_eq!(record.member_ordinal, ordinal as u32);
            assert_eq!(record.root_session, root_session);
            assert_eq!(
                record.state,
                if ordinal < 100 {
                    hya_store::AdmissionState::Accepted
                } else {
                    hya_store::AdmissionState::Queued
                }
            );
            assert!(record.actor.is_none());
        }
        assert!(
            records
                .iter()
                .all(|record| record.state != hya_store::AdmissionState::Started)
        );

        struct CountingSidecarEnvironment {
            calls: Arc<AtomicUsize>,
        }

        impl SidecarEnvironment for CountingSidecarEnvironment {
            fn factory_for(
                &self,
                _binding: &TurnBinding,
                _stable_id: &str,
            ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            }
        }

        let sidecar_calls = Arc::new(AtomicUsize::new(0));
        let sidecar_environment = CountingSidecarEnvironment {
            calls: Arc::clone(&sidecar_calls),
        };
        let changed_resolution = AdmissionResolutionContext::capture(
            AgentSpec {
                name: AgentName::new("build"),
                model: ModelRef::new("changed-base-model-sentinel"),
                system_prompt: "changed-base-system-prompt-sentinel".to_string(),
                workdir: workdir.clone(),
                reasoning: None,
            },
            Arc::clone(&categories),
            Arc::clone(&router),
        )
        .expect("changed admission context");
        let mismatch_ctx = AdmissionLaunchResolutionCtx {
            engine: &engine,
            binding: &binding,
            resolution: &changed_resolution,
            caller: "build",
            allowed_agents: request.agents.as_ref(),
            guidance: request.guidance.clone(),
            sidecar_environment: &sidecar_environment,
        };
        let mismatch_error = match resolve_admission_launches(
            &mismatch_ctx,
            vec![launches.first().cloned().expect("accepted launch")],
        ) {
            Ok(_) => panic!("mismatched admission context must fail closed"),
            Err(error) => error,
        };
        assert_eq!(mismatch_error, SpawnError::Unavailable);
        assert_eq!(sidecar_calls.load(Ordering::SeqCst), 0);

        let resolution_ctx = AdmissionLaunchResolutionCtx {
            engine: &engine,
            binding: &binding,
            resolution: &resolution,
            caller: "build",
            allowed_agents: request.agents.as_ref(),
            guidance: request.guidance.clone(),
            sidecar_environment: &sidecar_environment,
        };
        let mut corrupt_last_launch = launches.last().cloned().expect("accepted launch");
        corrupt_last_launch.intent.admission_binding_fingerprint[0] ^= 0x01;
        let corrupt_error = match resolve_admission_launches(
            &resolution_ctx,
            vec![
                launches.first().cloned().expect("accepted launch"),
                corrupt_last_launch,
            ],
        ) {
            Ok(_) => panic!("corrupt admission batch must fail closed"),
            Err(error) => error,
        };
        assert_eq!(corrupt_error, SpawnError::Unavailable);
        assert_eq!(sidecar_calls.load(Ordering::SeqCst), 0);
        let resolved = resolve_admission_launches(&resolution_ctx, launches)
            .expect("accepted launches must resolve in order");
        assert_eq!(resolved.len(), 100);
        for (ordinal, resolved_launch) in resolved.iter().enumerate() {
            let expected_intent = SpawnIntentV1::new(SpawnIntentInputV1 {
                member: request.members[ordinal].clone(),
                parent: request.parent,
                stable_target: AgentName::new("general"),
                background: request.background,
                operation: request.operation,
                member_ordinal: ordinal as u32,
                batch_cardinality: 101,
                prior_start: PriorStartV1::NeverStarted,
                runtime_fingerprint: resolved_launch.launch.intent.runtime_fingerprint,
                admission_binding_fingerprint: resolved_launch
                    .launch
                    .intent
                    .admission_binding_fingerprint,
                diagnostic_generation: binding.generation().get(),
            })
            .expect("expected accepted intent");
            assert_eq!(resolved_launch.launch.record.member_ordinal, ordinal as u32);
            assert_eq!(
                resolved_launch.launch.record.state,
                hya_store::AdmissionState::Accepted
            );
            assert_eq!(
                resolved_launch.launch.intent,
                expected_intent
                    .clone()
                    .into_admission_intent()
                    .expect("expected store intent")
            );
            assert_eq!(resolved_launch.intent, expected_intent);
            assert_eq!(
                resolved_launch.resolved.request.description,
                raw_members[ordinal].0
            );
            assert_eq!(
                resolved_launch.resolved.request.prompt,
                raw_members[ordinal].1
            );
            assert_eq!(
                resolved_launch.resolved.authorized_target.as_str(),
                "general"
            );
        }
        assert_eq!(sidecar_calls.load(Ordering::SeqCst), 100);
        let records_after = engine
            .store()
            .admissions(request.operation.operation_id())
            .await
            .expect("read rows after resolution");
        assert_eq!(records_after, records);
        assert!(
            records_after
                .iter()
                .all(|record| record.state != hya_store::AdmissionState::Started)
        );
    }

    #[tokio::test]
    async fn recovered_mismatch_aborts_once_and_resolves_promoted_match() {
        // Pins `HOME` for this test: it captures a runtime fingerprint in an
        // admission intent and recomputes it during resolution. See `ENV_LOCK`.
        let _env = StableEnvGuard::acquire();
        let workdir = tempdir().join("recovered-admission-resolution-workdir");
        std::fs::create_dir_all(&workdir).unwrap();
        let engine =
            engine_with_catalog(builtin_agent_catalog().expect("built-in catalog must load")).await;
        let binding = engine.bind_runtime(&workdir).expect("bind admission turn");
        let categories = Arc::new(CategoryRegistry::default());
        let router = Arc::new(ProviderRouter::new());
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("current-base-model"),
            system_prompt: "current-base-system-prompt".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let resolution = AdmissionResolutionContext::capture(
            base.clone(),
            Arc::clone(&categories),
            Arc::clone(&router),
        )
        .expect("current admission context");
        let runtime_fingerprint = engine
            .runtime_semantic_fingerprint_v1(&binding)
            .expect("runtime semantic fingerprint");
        let current_admission_fingerprint =
            resolution.admission_binding_fingerprint_v1(runtime_fingerprint);
        let changed_resolution = AdmissionResolutionContext::capture(
            AgentSpec {
                model: ModelRef::new("changed-base-model"),
                system_prompt: "current-base-system-prompt".to_string(),
                ..base.clone()
            },
            Arc::clone(&categories),
            Arc::clone(&router),
        )
        .expect("changed admission context");
        let changed_admission_fingerprint =
            changed_resolution.admission_binding_fingerprint_v1(runtime_fingerprint);
        assert_ne!(current_admission_fingerprint, changed_admission_fingerprint);
        let diagnostic_generation = binding
            .generation()
            .get()
            .checked_add(1)
            .expect("diagnostic generation increment");
        assert_ne!(diagnostic_generation, binding.generation().get());

        let root = SessionId::new();
        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let member = |ordinal: u32| SpawnMember {
            description: format!("recovered-description-{ordinal}"),
            prompt: format!("recovered-prompt-{ordinal}"),
            subagent_type: "general".to_string(),
            task_id: None,
            model: None,
            category: None,
            inline_agent: None,
            resident: false,
        };
        let intent = |ordinal: u32, admission_binding_fingerprint| {
            SpawnIntentV1::new(SpawnIntentInputV1 {
                member: member(ordinal),
                parent: root,
                stable_target: AgentName::new("general"),
                background: true,
                operation,
                member_ordinal: ordinal,
                batch_cardinality: 2,
                prior_start: PriorStartV1::NeverStarted,
                runtime_fingerprint,
                admission_binding_fingerprint,
                diagnostic_generation,
            })
            .expect("canonical recovered spawn intent")
        };
        let row0_spawn_intent = intent(0, changed_admission_fingerprint);
        let row1_spawn_intent = intent(1, current_admission_fingerprint);
        let row0_intent = row0_spawn_intent
            .clone()
            .into_admission_intent()
            .expect("row0 admission intent");
        let row1_intent = row1_spawn_intent
            .clone()
            .into_admission_intent()
            .expect("row1 admission intent");
        let claim = hya_store::AdmissionClaim {
            operation_id: operation.operation_id(),
            source_tool_call_id: operation.source_tool_call_id(),
            root_session: root,
            request_fingerprint: [0x71; 32],
            admission_units: 2,
            actor_claim: None,
        };
        let claimed = match engine
            .store()
            .claim_admission_batch(&claim, vec![row0_intent, row1_intent])
            .await
            .expect("claim recovered admission batch")
        {
            hya_store::AdmissionBatchClaimOutcome::Claimed(launches) => launches,
            hya_store::AdmissionBatchClaimOutcome::Existing => {
                panic!("fresh admission batch must not already exist")
            }
        };
        assert_eq!(claimed.len(), 2);
        assert_eq!(
            claimed
                .iter()
                .map(|launch| launch.record.member_ordinal)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(
            claimed
                .iter()
                .all(|launch| { launch.record.state == hya_store::AdmissionState::Accepted })
        );
        let mut recovered = engine
            .store()
            .recover_nonterminal_admissions("startup recovery")
            .await
            .expect("recover accepted admissions");
        recovered.sort_unstable_by_key(|record| record.member_ordinal);
        assert_eq!(recovered.len(), 2);
        assert_eq!(
            recovered
                .iter()
                .map(|record| record.member_ordinal)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(
            recovered
                .iter()
                .all(|record| record.state == hya_store::AdmissionState::Queued)
        );
        let launches = engine
            .store()
            .promote_queued_admissions(1)
            .await
            .expect("promote one recovered launch");
        assert_eq!(launches.len(), 1);
        let row0_launch = launches.into_iter().next().unwrap();
        assert_eq!(row0_launch.record.member_ordinal, 0);
        assert_eq!(
            row0_launch.record.state,
            hya_store::AdmissionState::Accepted
        );
        let stale_row0_launch = row0_launch.clone();

        struct CountingSidecarEnvironment {
            calls: Arc<AtomicUsize>,
        }

        impl SidecarEnvironment for CountingSidecarEnvironment {
            fn factory_for(
                &self,
                _binding: &TurnBinding,
                _stable_id: &str,
            ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            }
        }

        let sidecar_calls = Arc::new(AtomicUsize::new(0));
        let sidecar_environment = CountingSidecarEnvironment {
            calls: Arc::clone(&sidecar_calls),
        };
        let allowed_agents: Arc<[AgentDef]> = vec![AgentDef {
            name: "general".to_string(),
            description: None,
            category: None,
            mode: "subagent".to_string(),
        }]
        .into();
        let resolution_ctx = AdmissionLaunchResolutionCtx {
            engine: &engine,
            binding: &binding,
            resolution: &resolution,
            caller: "build",
            allowed_agents: allowed_agents.as_ref(),
            guidance: None,
            sidecar_environment: &sidecar_environment,
        };
        assert_eq!(sidecar_calls.load(Ordering::SeqCst), 0);
        let resolved = resolve_current_admission_launches(&resolution_ctx, row0_launch, None)
            .await
            .expect("mismatched recovered launch must resolve its promotion");
        assert_eq!(resolved.len(), 1);
        let resolved_row1 = resolved.into_iter().next().unwrap();
        assert_eq!(resolved_row1.launch.record.member_ordinal, 1);
        assert_eq!(
            resolved_row1.launch.record.state,
            hya_store::AdmissionState::Accepted
        );
        assert_eq!(resolved_row1.intent, row1_spawn_intent);
        assert_eq!(
            resolved_row1.resolved.authorized_target,
            AgentName::new("general")
        );
        assert_eq!(
            resolved_row1.resolved.binding.generation(),
            binding.generation()
        );
        assert_eq!(sidecar_calls.load(Ordering::SeqCst), 1);

        let rows = engine
            .store()
            .admissions(operation.operation_id())
            .await
            .unwrap();
        let row0_record = rows
            .iter()
            .find(|record| record.member_ordinal == 0)
            .unwrap();
        assert_eq!(row0_record.state, hya_store::AdmissionState::Aborted);
        assert_eq!(
            row0_record.terminal_reason.as_deref(),
            Some("admission recovery resolution unavailable")
        );
        let row1_record = rows
            .iter()
            .find(|record| record.member_ordinal == 1)
            .unwrap();
        assert_eq!(row1_record.state, hya_store::AdmissionState::Accepted);
        let counts = engine.store().admission_counts().await.unwrap();
        assert_eq!(counts.active, 1);
        assert_eq!(counts.non_active, 0);
        assert_eq!(counts.total, 1);

        let work_count = Arc::new(AtomicUsize::new(0));
        let work_count_for_task = Arc::clone(&work_count);
        let observed_store = engine.store().clone();
        let work_future = async move {
            work_count_for_task.fetch_add(1, Ordering::SeqCst);
            observed_store
                .admissions(operation.operation_id())
                .await
                .expect("row1 admission remains readable")
                .into_iter()
                .find(|record| record.member_ordinal == 1)
                .map(|record| record.state)
        };
        let installed = install_admission_task(&resolved_row1.launch, work_future);
        tokio::task::yield_now().await;
        assert_eq!(work_count.load(Ordering::SeqCst), 0);
        let row1_handle = installed
            .start(engine.store(), None)
            .await
            .expect("promoted row1 must start through the existing CAS barrier");
        let observed_state = row1_handle.await.expect("row1 task must finish");
        assert_eq!(observed_state, Some(hya_store::AdmissionState::Started));
        assert_eq!(work_count.load(Ordering::SeqCst), 1);
        let started_rows = engine
            .store()
            .admissions(operation.operation_id())
            .await
            .unwrap();
        assert_eq!(
            started_rows
                .iter()
                .find(|record| record.member_ordinal == 1)
                .unwrap()
                .state,
            hya_store::AdmissionState::Started
        );

        let before_replay = engine
            .store()
            .admissions(operation.operation_id())
            .await
            .unwrap();
        let before_counts = engine.store().admission_counts().await.unwrap();
        let before_sidecar_calls = sidecar_calls.load(Ordering::SeqCst);
        assert!(
            resolve_current_admission_launches(&resolution_ctx, stale_row0_launch, None)
                .await
                .expect("stale aborted launch must be ignored")
                .is_empty()
        );
        assert!(
            resolve_current_admission_launches(&resolution_ctx, resolved_row1.launch.clone(), None)
                .await
                .expect("stale started launch must be ignored")
                .is_empty()
        );
        assert_eq!(sidecar_calls.load(Ordering::SeqCst), before_sidecar_calls);
        assert_eq!(work_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            engine
                .store()
                .admissions(operation.operation_id())
                .await
                .unwrap(),
            before_replay
        );
        assert_eq!(
            engine.store().admission_counts().await.unwrap(),
            before_counts
        );
    }

    #[tokio::test]
    async fn recovered_promotions_reconstruct_each_parent_binding() {
        // Pins `HOME` for this test: it captures a runtime fingerprint in an
        // admission intent and recomputes it during resolution. See `ENV_LOCK`.
        let _env = StableEnvGuard::acquire();
        let workdir_a = tempdir().join("recovered-promotion-parent-a");
        let workdir_b = tempdir().join("recovered-promotion-parent-b");
        std::fs::create_dir_all(&workdir_a).unwrap();
        std::fs::create_dir_all(&workdir_b).unwrap();
        write_skill(
            &workdir_a.join(".hya/skills/runtime-a"),
            "runtime-a",
            "runtime A skill",
            "runtime A skill body",
        );
        write_skill(
            &workdir_b.join(".hya/skills/runtime-b"),
            "runtime-b",
            "runtime B skill",
            "runtime B skill body",
        );

        let engine = engine_with_catalog(builtin_agent_catalog().unwrap()).await;
        let parent_a = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: workdir_a.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let parent_b = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: workdir_b.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        assert_ne!(parent_a, parent_b);

        let binding_a = engine.bind_runtime(&workdir_a).unwrap();
        let binding_b = engine.bind_runtime(&workdir_b).unwrap();
        assert_eq!(binding_a.workdir(), workdir_a);
        assert_eq!(binding_b.workdir(), workdir_b);
        let runtime_fingerprint_a = engine.runtime_semantic_fingerprint_v1(&binding_a).unwrap();
        let runtime_fingerprint_b = engine.runtime_semantic_fingerprint_v1(&binding_b).unwrap();
        assert_ne!(runtime_fingerprint_a, runtime_fingerprint_b);

        let categories = Arc::new(CategoryRegistry::default());
        let router = Arc::new(ProviderRouter::new());
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("current-base-model"),
            system_prompt: "current-base-system-prompt".to_string(),
            workdir: workdir_a.clone(),
            reasoning: None,
        };
        let resolution = AdmissionResolutionContext::capture(
            base.clone(),
            Arc::clone(&categories),
            Arc::clone(&router),
        )
        .unwrap();
        let admission_fingerprint_a =
            resolution.admission_binding_fingerprint_v1(runtime_fingerprint_a);
        let admission_fingerprint_b =
            resolution.admission_binding_fingerprint_v1(runtime_fingerprint_b);
        let changed_resolution = AdmissionResolutionContext::capture(
            AgentSpec {
                model: ModelRef::new("changed-base-model"),
                ..base
            },
            Arc::clone(&categories),
            Arc::clone(&router),
        )
        .unwrap();
        let changed_admission_fingerprint_a =
            changed_resolution.admission_binding_fingerprint_v1(runtime_fingerprint_a);
        assert_ne!(admission_fingerprint_a, changed_admission_fingerprint_a);

        let member = |parent: SessionId, ordinal: u32| SpawnMember {
            description: format!("recovered-parent-{parent}-description-{ordinal}"),
            prompt: format!("recovered-parent-{parent}-prompt-{ordinal}"),
            subagent_type: "general".to_string(),
            task_id: None,
            model: None,
            category: None,
            inline_agent: None,
            resident: false,
        };
        let operation_a = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let intents_a = (0..100)
            .map(|ordinal| {
                SpawnIntentV1::new(SpawnIntentInputV1 {
                    member: member(parent_a, ordinal),
                    parent: parent_a,
                    stable_target: AgentName::new("general"),
                    background: true,
                    operation: operation_a,
                    member_ordinal: ordinal,
                    batch_cardinality: 100,
                    prior_start: PriorStartV1::NeverStarted,
                    runtime_fingerprint: runtime_fingerprint_a,
                    admission_binding_fingerprint: changed_admission_fingerprint_a,
                    diagnostic_generation: binding_a.generation().get(),
                })
                .unwrap()
                .into_admission_intent()
                .unwrap()
            })
            .collect::<Vec<_>>();
        let claim_a = hya_store::AdmissionClaim {
            operation_id: operation_a.operation_id(),
            source_tool_call_id: operation_a.source_tool_call_id(),
            root_session: parent_a,
            request_fingerprint: [0xa1; 32],
            admission_units: 100,
            actor_claim: None,
        };
        let launches_a = match engine
            .store()
            .claim_admission_batch(&claim_a, intents_a)
            .await
            .unwrap()
        {
            hya_store::AdmissionBatchClaimOutcome::Claimed(launches) => launches,
            hya_store::AdmissionBatchClaimOutcome::Existing => {
                panic!("fresh parent A admission batch must not already exist")
            }
        };
        assert_eq!(launches_a.len(), 100);
        assert!(
            launches_a
                .iter()
                .all(|launch| { launch.record.state == hya_store::AdmissionState::Accepted })
        );
        let row0_launch = launches_a.first().cloned().unwrap();
        assert_eq!(row0_launch.record.member_ordinal, 0);

        let operation_b = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let request_fingerprint_a = [0xa1; 32];
        let request_fingerprint_b = [0xb1; 32];
        assert_ne!(operation_a.operation_id(), operation_b.operation_id());
        assert_ne!(request_fingerprint_a, request_fingerprint_b);
        let stale_generation_b = binding_b.generation().get().checked_add(1).unwrap();
        assert_ne!(stale_generation_b, binding_b.generation().get());
        let b_spawn_intent = SpawnIntentV1::new(SpawnIntentInputV1 {
            member: member(parent_b, 0),
            parent: parent_b,
            stable_target: AgentName::new("general"),
            background: true,
            operation: operation_b,
            member_ordinal: 0,
            batch_cardinality: 1,
            prior_start: PriorStartV1::NeverStarted,
            runtime_fingerprint: runtime_fingerprint_b,
            admission_binding_fingerprint: admission_fingerprint_b,
            diagnostic_generation: stale_generation_b,
        })
        .unwrap();
        let claim_b = hya_store::AdmissionClaim {
            operation_id: operation_b.operation_id(),
            source_tool_call_id: operation_b.source_tool_call_id(),
            root_session: parent_b,
            request_fingerprint: request_fingerprint_b,
            admission_units: 1,
            actor_claim: None,
        };
        let launches_b = match engine
            .store()
            .claim_admission_batch(
                &claim_b,
                vec![b_spawn_intent.clone().into_admission_intent().unwrap()],
            )
            .await
            .unwrap()
        {
            hya_store::AdmissionBatchClaimOutcome::Claimed(launches) => launches,
            hya_store::AdmissionBatchClaimOutcome::Existing => {
                panic!("fresh parent B admission batch must not already exist")
            }
        };
        assert!(launches_b.is_empty());
        let records_b = engine
            .store()
            .admissions(operation_b.operation_id())
            .await
            .unwrap();
        assert_eq!(records_b.len(), 1);
        assert_eq!(records_b[0].state, hya_store::AdmissionState::Queued);

        let sessions_before = engine.store().list_sessions().await.unwrap();
        assert_eq!(sessions_before.len(), 2);
        assert!(
            sessions_before
                .iter()
                .all(|session| session.session == parent_a || session.session == parent_b)
        );

        struct CountingSidecarEnvironment {
            calls: Arc<AtomicUsize>,
        }

        impl SidecarEnvironment for CountingSidecarEnvironment {
            fn factory_for(
                &self,
                _binding: &TurnBinding,
                _stable_id: &str,
            ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            }
        }

        let sidecar_calls = Arc::new(AtomicUsize::new(0));
        let sidecar_environment = CountingSidecarEnvironment {
            calls: Arc::clone(&sidecar_calls),
        };
        let resolved = resolve_recovered_admission_launches(
            &engine,
            &resolution,
            &sidecar_environment,
            row0_launch,
        )
        .await
        .unwrap();
        assert_eq!(resolved.len(), 1);
        let resolved_b = &resolved[0];
        assert_eq!(
            resolved_b.launch.record.operation_id,
            operation_b.operation_id()
        );
        assert_eq!(resolved_b.launch.record.member_ordinal, 0);
        assert_eq!(resolved_b.intent, b_spawn_intent);
        assert_eq!(
            resolved_b.resolved.authorized_target,
            AgentName::new("general")
        );
        assert_eq!(resolved_b.resolved.binding.workdir(), workdir_b);
        assert_eq!(
            resolved_b.launch.intent.runtime_fingerprint,
            runtime_fingerprint_b
        );
        assert_ne!(
            resolved_b.launch.intent.runtime_fingerprint,
            runtime_fingerprint_a
        );
        assert_eq!(sidecar_calls.load(Ordering::SeqCst), 1);

        let records_a = engine
            .store()
            .admissions(operation_a.operation_id())
            .await
            .unwrap();
        assert_eq!(records_a.len(), 100);
        assert_eq!(records_a[0].state, hya_store::AdmissionState::Aborted);
        assert_eq!(
            records_a[0].terminal_reason.as_deref(),
            Some("admission recovery resolution unavailable")
        );
        assert!(
            records_a[1..]
                .iter()
                .all(|record| record.state == hya_store::AdmissionState::Accepted)
        );
        let records_b = engine
            .store()
            .admissions(operation_b.operation_id())
            .await
            .unwrap();
        assert_eq!(records_b.len(), 1);
        assert_eq!(records_b[0].state, hya_store::AdmissionState::Accepted);
        assert_eq!(
            engine.store().admission_counts().await.unwrap(),
            hya_store::AdmissionCounts {
                active: 100,
                non_active: 0,
                total: 100,
            }
        );

        let sessions_after = engine.store().list_sessions().await.unwrap();
        assert_eq!(sessions_after.len(), 2);
        assert_eq!(
            sessions_before
                .iter()
                .map(|session| session.session.to_string())
                .collect::<BTreeSet<_>>(),
            sessions_after
                .iter()
                .map(|session| session.session.to_string())
                .collect::<BTreeSet<_>>()
        );
    }

    #[tokio::test]
    async fn recovered_transient_launch_executes_only_after_started_barrier() {
        // Pins `HOME` for this test: it captures a runtime fingerprint in an
        // admission intent and recomputes it during resolution. See `ENV_LOCK`.
        let _env = StableEnvGuard::acquire();
        let workdir = tempdir().join("recovered-transient-admission-workdir");
        std::fs::create_dir_all(&workdir).unwrap();
        struct IdentityFakeProvider {
            configured_identity_calls: Arc<AtomicUsize>,
            inner: FakeProvider,
        }

        #[async_trait]
        impl Provider for IdentityFakeProvider {
            fn id(&self) -> &str {
                self.inner.id()
            }

            fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
                self.inner.capabilities(model)
            }

            fn configured_identity_v1(&self) -> Option<Vec<u8>> {
                self.configured_identity_calls
                    .fetch_add(1, Ordering::SeqCst);
                Some(b"hya-test-configured-identity-v1".to_vec())
            }

            async fn stream(
                &self,
                request: CompletionRequest,
                session: SessionId,
                message: hya_proto::MessageId,
            ) -> Result<EventStream, ProviderError> {
                self.inner.stream(request, session, message).await
            }
        }

        let configured_identity_calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(IdentityFakeProvider {
            configured_identity_calls: Arc::clone(&configured_identity_calls),
            inner: FakeProvider::scripted_turns(vec![
                vec![
                    FakeStep::ToolCall {
                        name: "task".to_string(),
                        input: json!({
                            "description": "recovered nested task",
                            "prompt": "complete the nested task",
                            "subagent_type": "general"
                        }),
                    },
                    FakeStep::Finish(FinishReason::ToolCalls),
                ],
                vec![FakeStep::Finish(FinishReason::Stop)],
            ]),
        });
        let router = Arc::new(ProviderRouter::new().with(provider));
        let runtime = Arc::new(RuntimeRegistry::new(
            ToolRegistry::builtins(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _permission_rx) =
            PermissionPlane::new(PermissionRules::new(vec![Rule::new(
                Action::Task,
                "*",
                Mode::Allow,
            )]));
        let (spawn_sender, mut spawn_rx) = BoundSpawnSender::with_capacity(1);
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender),
        );
        let parent = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let runtime_fingerprint = engine.runtime_semantic_fingerprint_v1(&binding).unwrap();
        let categories = Arc::new(CategoryRegistry::default());
        let resolution = AdmissionResolutionContext::capture(
            AgentSpec {
                name: AgentName::new("build"),
                model: ModelRef::new("fake"),
                system_prompt: "current prompt".to_string(),
                workdir: workdir.clone(),
                reasoning: None,
            },
            Arc::clone(&categories),
            Arc::clone(&router),
        )
        .unwrap();
        assert_eq!(
            configured_identity_calls.load(Ordering::SeqCst),
            1,
            "AdmissionResolutionContext::capture must use the uniquely observable engine provider"
        );
        let admission_fingerprint =
            resolution.admission_binding_fingerprint_v1(runtime_fingerprint);
        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let spawn_intent = SpawnIntentV1::new(SpawnIntentInputV1 {
            member: SpawnMember {
                description: "recovered transient description".to_string(),
                prompt: "recovered transient prompt".to_string(),
                subagent_type: "general".to_string(),
                task_id: None,
                model: None,
                category: None,
                inline_agent: None,
                resident: false,
            },
            parent,
            stable_target: AgentName::new("general"),
            background: true,
            operation,
            member_ordinal: 0,
            batch_cardinality: 1,
            prior_start: PriorStartV1::NeverStarted,
            runtime_fingerprint,
            admission_binding_fingerprint: admission_fingerprint,
            diagnostic_generation: binding.generation().get(),
        })
        .unwrap();
        let claim = hya_store::AdmissionClaim {
            operation_id: operation.operation_id(),
            source_tool_call_id: operation.source_tool_call_id(),
            root_session: parent,
            request_fingerprint: [0x91; 32],
            admission_units: 1,
            actor_claim: None,
        };
        let launch = match engine
            .store()
            .claim_admission_batch(
                &claim,
                vec![spawn_intent.clone().into_admission_intent().unwrap()],
            )
            .await
            .unwrap()
        {
            hya_store::AdmissionBatchClaimOutcome::Claimed(mut launches) => {
                assert_eq!(launches.len(), 1);
                launches.pop().unwrap()
            }
            hya_store::AdmissionBatchClaimOutcome::Existing => {
                panic!("fresh transient admission must not already exist")
            }
        };
        assert_eq!(launch.record.state, hya_store::AdmissionState::Accepted);
        let stale_launch = launch.clone();

        struct CountingSidecarEnvironment {
            calls: Arc<AtomicUsize>,
        }

        impl SidecarEnvironment for CountingSidecarEnvironment {
            fn factory_for(
                &self,
                _binding: &TurnBinding,
                _stable_id: &str,
            ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            }
        }

        let sidecar_calls = Arc::new(AtomicUsize::new(0));
        let sidecar_environment = CountingSidecarEnvironment {
            calls: Arc::clone(&sidecar_calls),
        };
        let mut resolved = resolve_recovered_admission_launches(
            &engine,
            &resolution,
            &sidecar_environment,
            launch,
        )
        .await
        .unwrap();
        assert_eq!(resolved.len(), 1);
        let resolved_launch = resolved.pop().unwrap();
        assert_eq!(sidecar_calls.load(Ordering::SeqCst), 1);

        let sessions_before = engine.store().list_sessions().await.unwrap();
        assert_eq!(sessions_before.len(), 1);
        let parent_before = engine.read_projection(parent).await.unwrap();
        assert!(parent_before.session.members.is_empty());
        let record_before = engine
            .store()
            .admission(operation.operation_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record_before.state, hya_store::AdmissionState::Accepted);
        assert!(!record_before.logical_released);

        let installed = install_transient_admission_launch(
            Arc::clone(&engine),
            resolved_launch,
            CancellationToken::new(),
            None,
        );
        assert!(matches!(
            spawn_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(engine.store().list_sessions().await.unwrap().len(), 1);
        assert!(
            engine
                .read_projection(parent)
                .await
                .unwrap()
                .session
                .members
                .is_empty()
        );
        let record_before_start = engine
            .store()
            .admission(operation.operation_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            record_before_start.state,
            hya_store::AdmissionState::Accepted
        );
        assert!(!record_before_start.logical_released);
        assert_eq!(sidecar_calls.load(Ordering::SeqCst), 1);

        let handle = installed.start(engine.store(), None).await.unwrap();
        let bound_request =
            tokio::time::timeout(std::time::Duration::from_secs(5), spawn_rx.recv())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            bound_request.parent_admission(),
            Some(hya_core::AdmissionMemberIdentity {
                operation_id: operation.operation_id(),
                member_ordinal: 0,
            })
        );
        let (_nested_binding, nested_request) = bound_request.into_parts();
        let nested_parent = nested_request.parent;
        let nested_session = SessionId::new();
        nested_request
            .reply
            .send(Ok(vec![MemberOutcome {
                member: "nested-member".to_string(),
                session: nested_session.to_string(),
                status: "done".to_string(),
                summary: "nested task complete".to_string(),
            }]))
            .unwrap();
        let completion = handle.await.unwrap().unwrap();
        assert_eq!(completion.operation_id, operation.operation_id());
        assert_eq!(completion.member_ordinal, 0);
        assert_eq!(completion.evidence.status, MemberStatus::Done);
        assert_ne!(completion.evidence.session, "-");
        let child = completion.evidence.session.parse::<SessionId>().unwrap();
        assert_eq!(nested_parent, child);
        assert_eq!(completion.promoted.len(), 0);

        let completed_record = engine
            .store()
            .admission(operation.operation_id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed_record.state, hya_store::AdmissionState::Completed);
        assert!(completed_record.logical_released);
        assert_eq!(
            completed_record.terminal_reason.as_deref(),
            Some("spawn member completed")
        );
        assert_eq!(
            engine.store().admission_counts().await.unwrap(),
            hya_store::AdmissionCounts {
                active: 0,
                non_active: 0,
                total: 0,
            }
        );

        let sessions_after = engine.store().list_sessions().await.unwrap();
        assert_eq!(sessions_after.len(), 2);
        let child_projection = engine.read_projection(child).await.unwrap();
        assert_eq!(child_projection.session.parent, Some(parent));
        let parent_after = engine.read_projection(parent).await.unwrap();
        assert_eq!(parent_after.session.members.len(), 1);
        assert_eq!(
            parent_after.session.members[0].status,
            MemberRunStatus::Done
        );
        assert_eq!(parent_after.session.members[0].child, Some(child));
        assert_eq!(sidecar_calls.load(Ordering::SeqCst), 1);

        assert!(
            resolve_recovered_admission_launches(
                &engine,
                &resolution,
                &sidecar_environment,
                stale_launch,
            )
            .await
            .unwrap()
            .is_empty()
        );
        assert_eq!(sidecar_calls.load(Ordering::SeqCst), 1);
        assert_eq!(engine.store().list_sessions().await.unwrap().len(), 2);
        assert_eq!(
            engine
                .read_projection(parent)
                .await
                .unwrap()
                .session
                .members
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn accepted_admission_task_executes_only_after_started_commit() {
        let database = tempdir().join("accepted-admission-task.db");
        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
        let engine = SessionEngine::new(
            store.clone(),
            Arc::new(ProviderRouter::new()),
            runtime,
            permission,
            EventBus::default(),
        );
        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let root_session = SessionId::new();
        let intent = SpawnIntentV1::new(SpawnIntentInputV1 {
            member: SpawnMember {
                description: "accepted-admission-task-description".to_string(),
                prompt: "accepted-admission-task-prompt".to_string(),
                subagent_type: "general".to_string(),
                task_id: None,
                model: None,
                category: None,
                inline_agent: None,
                resident: false,
            },
            parent: root_session,
            stable_target: AgentName::new("general"),
            background: true,
            operation,
            member_ordinal: 0,
            batch_cardinality: 1,
            prior_start: PriorStartV1::NeverStarted,
            runtime_fingerprint: [0x11; 32],
            admission_binding_fingerprint: [0x22; 32],
            diagnostic_generation: 7,
        })
        .expect("one-member intent must be canonical")
        .into_admission_intent()
        .expect("one-member intent must encode");
        let claim = hya_store::AdmissionClaim {
            operation_id: operation.operation_id(),
            source_tool_call_id: operation.source_tool_call_id(),
            root_session,
            request_fingerprint: [0x33; 32],
            admission_units: 1,
            actor_claim: None,
        };
        let launches = match store
            .claim_admission_batch(&claim, vec![intent])
            .await
            .expect("one-member admission claim must succeed")
        {
            hya_store::AdmissionBatchClaimOutcome::Claimed(launches) => launches,
            hya_store::AdmissionBatchClaimOutcome::Existing => {
                panic!("fresh temporary store must claim one admission launch")
            }
        };
        assert_eq!(launches.len(), 1);
        let launch = launches.into_iter().next().unwrap();
        assert_eq!(launch.record.state, hya_store::AdmissionState::Accepted);
        assert!(launch.record.actor.is_none());

        let work_polls = Arc::new(AtomicUsize::new(0));
        let observed_store = store.clone();
        let observed_operation = operation.operation_id();
        let work_polls_for_task = Arc::clone(&work_polls);
        let work_future = async move {
            work_polls_for_task.fetch_add(1, Ordering::SeqCst);
            observed_store
                .admission(observed_operation)
                .await
                .expect("admission row must remain readable")
                .map(|record| record.state)
        };
        let installed = install_admission_task(&launch, work_future);

        tokio::task::yield_now().await;
        assert_eq!(work_polls.load(Ordering::SeqCst), 0);
        let before_start = store
            .admission(operation.operation_id())
            .await
            .expect("read accepted admission row")
            .expect("accepted admission row must exist");
        assert_eq!(before_start.state, hya_store::AdmissionState::Accepted);
        assert!(before_start.actor.is_none());

        let handle = installed
            .start(engine.store(), None)
            .await
            .expect("starting accepted admission task must commit the barrier");
        let observed_state = handle.await.expect("admission task must finish");
        assert_eq!(observed_state, Some(hya_store::AdmissionState::Started));
        assert_eq!(work_polls.load(Ordering::SeqCst), 1);
        let after_start = store
            .admission(operation.operation_id())
            .await
            .expect("read started admission row")
            .expect("started admission row must exist");
        assert_eq!(after_start.state, hya_store::AdmissionState::Started);
        assert!(after_start.actor.is_none());
    }

    #[tokio::test]
    async fn failure_quiesces_real_member_tasks_before_cleanup_reply() {
        struct QuiescedGuard(Arc<AtomicBool>);

        impl Drop for QuiescedGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let engine = Arc::new(
            engine_with_catalog(builtin_agent_catalog().expect("built-in catalog must load")).await,
        );
        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_id = operation.operation_id();
        let root_session = SessionId::new();
        let launch = match engine
            .store()
            .claim_admission_batch(
                &hya_store::AdmissionClaim {
                    operation_id,
                    source_tool_call_id: operation.source_tool_call_id(),
                    root_session,
                    request_fingerprint: [0x41; 32],
                    admission_units: 1,
                    actor_claim: None,
                },
                vec![hya_store::AdmissionIntent {
                    runtime_fingerprint_version: 1,
                    runtime_fingerprint: [0x42; 32],
                    admission_binding_fingerprint_version: 1,
                    admission_binding_fingerprint: [0x43; 32],
                    spawn_intent: vec![0x44],
                }],
            )
            .await
            .expect("one-member admission claim must succeed")
        {
            hya_store::AdmissionBatchClaimOutcome::Claimed(mut launches) => {
                assert_eq!(launches.len(), 1);
                launches.pop().unwrap()
            }
            hya_store::AdmissionBatchClaimOutcome::Existing => {
                panic!("fresh in-memory admission must not already exist")
            }
        };
        assert_eq!(launch.record.state, hya_store::AdmissionState::Accepted);
        engine
            .store()
            .start_admission_member(operation_id, 0, None)
            .await
            .expect("claimed member must transition to started");

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let quiesced = Arc::new(AtomicBool::new(false));
        let post_cleanup_work = Arc::new(AtomicUsize::new(0));
        let operation_cancel = CancellationToken::new();
        let task_entered = Arc::clone(&entered);
        let task_release = Arc::clone(&release);
        let task_quiesced = Arc::clone(&quiesced);
        let task_post_cleanup_work = Arc::clone(&post_cleanup_work);
        let real_task: tokio::task::JoinHandle<Result<TransientAdmissionCompletion, SpawnError>> =
            tokio::spawn(async move {
                let _guard = QuiescedGuard(task_quiesced);
                task_entered.notify_one();
                task_release.notified().await;
                task_post_cleanup_work.fetch_add(1, Ordering::SeqCst);
                Err(SpawnError::Unavailable)
            });
        let real_abort = real_task.abort_handle();
        let mut handles = vec![real_task];

        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("controlled member task did not enter");

        let (reply, reply_rx) =
            tokio::sync::oneshot::channel::<Result<Vec<MemberOutcome>, SpawnError>>();
        let cleanup_engine = Arc::clone(&engine);
        let cleanup_cancel = operation_cancel.clone();
        let cleanup_task = tokio::spawn(async move {
            let cleaned = cleanup_transient_admission(
                &cleanup_engine,
                operation_id,
                None,
                &cleanup_cancel,
                &mut handles,
                1,
                "forced RED1b cleanup",
                None,
            )
            .await;
            assert!(cleaned, "direct-handle cleanup must be provable");
            let _ = reply.send(Err(SpawnError::Unavailable));
        });

        let store = engine.store().clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if store
                    .admission(operation_id)
                    .await
                    .unwrap()
                    .is_some_and(|record| record.state == hya_store::AdmissionState::Aborted)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("durable cleanup did not commit Aborted");

        assert!(
            quiesced.load(Ordering::SeqCst),
            "durable cleanup committed before real member task quiesced"
        );

        cleanup_task.await.expect("cleanup task must not panic");
        assert!(real_abort.is_finished());
        assert!(quiesced.load(Ordering::SeqCst));
        assert!(matches!(
            reply_rx.await.expect("cleanup must send one reply"),
            Err(SpawnError::Unavailable)
        ));

        release.notify_one();
        tokio::time::timeout(Duration::from_millis(250), async {
            loop {
                if real_abort.is_finished() && post_cleanup_work.load(Ordering::SeqCst) == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("real member task did not remain quiescent after cleanup");
        assert_eq!(post_cleanup_work.load(Ordering::SeqCst), 0);
    }

    #[derive(Clone, Copy, Debug)]
    enum PostClaimFailureKind {
        GovernorOverload,
        AcceptedResolution,
    }

    #[derive(Clone, Copy, Debug)]
    enum CleanupFault {
        Healthy,
        RejectAbort,
        DeleteAbort,
    }

    async fn post_claim_failure_case(
        kind: PostClaimFailureKind,
        fault: CleanupFault,
        check_order: bool,
    ) {
        let cardinality = 3_u32;
        let per_run_budget = match kind {
            PostClaimFailureKind::GovernorOverload => 2_u64,
            PostClaimFailureKind::AcceptedResolution => u64::from(cardinality),
        };
        let workdir = tempdir().join("post-claim-failure-workdir");
        std::fs::create_dir_all(&workdir).unwrap();
        let database_path = tempdir().join("post-claim-failure.sqlite3");
        let store = match fault {
            CleanupFault::Healthy => SessionStore::connect_memory().await.unwrap(),
            CleanupFault::RejectAbort | CleanupFault::DeleteAbort => {
                SessionStore::connect(database_path.to_str().unwrap())
                    .await
                    .unwrap()
            }
        };
        if !matches!(fault, CleanupFault::Healthy) {
            let mut connection =
                SqliteConnection::connect(&format!("sqlite://{}", database_path.display()))
                    .await
                    .unwrap();
            let trigger = match fault {
                CleanupFault::RejectAbort => {
                    "CREATE TRIGGER test_post_claim_reject_abort
                     BEFORE UPDATE OF state ON admission_journal
                     WHEN NEW.state = 'aborted'
                     BEGIN SELECT RAISE(ABORT, 'test post-claim cleanup rejection'); END;"
                }
                CleanupFault::DeleteAbort => {
                    "CREATE TRIGGER test_post_claim_delete_abort
                     AFTER UPDATE OF state ON admission_journal
                     WHEN NEW.state = 'aborted'
                     BEGIN DELETE FROM admission_journal
                       WHERE operation_id = NEW.operation_id
                         AND member_ordinal = NEW.member_ordinal; END;"
                }
                CleanupFault::Healthy => unreachable!(),
            };
            sqlx::query(trigger).execute(&mut connection).await.unwrap();
            connection.close().await.unwrap();
        }

        let catalog = {
            let prepared = prepare_package(BundleSource::new(
                "post-claim-resolution",
                vec![
                    SourceFile::new(
                        "bundle.yaml",
                        br#"kind: AgentBundle
identity:
  id: hya/post-claim-resolution
  version: 0.0.1
  publisher: hya-tests
resources:
  tools:
    - id: echo
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agent:
  id: worker
  role: subagent
  spawn_lifecycle: transient
  resource_view:
    allow:
      - echo
  prompt: prompts/worker.md
"#
                        .to_vec(),
                    ),
                    SourceFile::new("prompts/worker.md", b"post-claim worker prompt"),
                    SourceFile::new(
                        "extensions/runtime.js",
                        b"export default {};
",
                    ),
                ],
            ))
            .expect("selected-sidecar fixture must prepare");
            Arc::new(to_agent_catalog(
                BundleCatalog::from_verified_catalogs(&[&prepared])
                    .expect("selected-sidecar fixture must retain semantic identity"),
            ))
        };
        let router = Arc::new(ProviderRouter::new());
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            catalog,
        ));
        let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
        let engine = Arc::new(
            SessionEngine::new(
                store,
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_governor(SubagentGovernor::new(hya_core::SubagentLimits {
                per_run_budget,
                ..hya_core::SubagentLimits::default()
            })),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "post-claim base prompt".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();
        assert!(
            agents.iter().any(|agent| agent.name == "worker"),
            "worker must be authorized in the selected-sidecar fixture"
        );

        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_id = operation.operation_id();
        let members = (0..cardinality)
            .map(|ordinal| SpawnMember {
                description: format!("post-claim failure member {ordinal}"),
                prompt: format!("post-claim failure prompt {ordinal}"),
                subagent_type: "worker".to_string(),
                ..SpawnMember::default()
            })
            .collect();
        let (reply, mut reply_rx) = tokio::sync::oneshot::channel();
        let request = SpawnRequest {
            parent,
            agents,
            guidance: None,
            operation,
            members,
            cancel: CancellationToken::new(),
            background: false,
            reply,
        };
        let test_observer = Arc::new(AdmissionTestObserver::new());
        let cleanup_notified = test_observer.cleanup_attempt.notified();
        let cleanup_finished = test_observer.cleanup_finished.notified();
        let sidecar_environment = Arc::new(BundleSidecarEnvironment {
            command: None,
            staging_root: tempdir(),
            terminate_notify: None,
            test_observer: Some(Arc::clone(&test_observer)),
            uniform_probe: None,
        });
        let wake_router = Arc::new(ForegroundAdmissionWakeRouter::new(Some(Arc::clone(
            &test_observer,
        ))));
        let preparation = ForegroundTransientAdmissionPreparation {
            engine: Arc::clone(&engine),
            binding,
            base,
            router,
            categories: Arc::new(CategoryRegistry::default()),
            sidecar_environment,
            caller: "build".to_string(),
            req: request,
            wake_router,
            reply_mode: DurableOwnerReplyMode::ForegroundWholeBatch,
        };
        let preparation_task = if matches!(fault, CleanupFault::Healthy) {
            preparation.run().await;
            None
        } else {
            Some(tokio::spawn(preparation.run()))
        };

        let cleanup_observed = if matches!(fault, CleanupFault::Healthy) {
            let reply = tokio::time::timeout(Duration::from_secs(1), &mut reply_rx)
                .await
                .expect("successful cleanup must send a typed reply")
                .expect("reply sender must remain available");
            match kind {
                PostClaimFailureKind::GovernorOverload => {
                    assert!(matches!(reply, Err(SpawnError::Overloaded)));
                }
                PostClaimFailureKind::AcceptedResolution => {
                    assert!(matches!(reply, Err(SpawnError::Unavailable)));
                }
            }
            true
        } else {
            let attempt_observed = tokio::time::timeout(Duration::from_secs(1), cleanup_notified)
                .await
                .is_ok();
            let finished_observed = tokio::time::timeout(Duration::from_secs(1), cleanup_finished)
                .await
                .is_ok();
            assert!(
                attempt_observed && finished_observed,
                "cleanup attempt and its durable proof result must be observed before checking the caller reply"
            );
            true
        };

        if check_order {
            let ordered_steps = test_observer.sequence.load(Ordering::SeqCst);
            assert_eq!(
                ordered_steps,
                match kind {
                    PostClaimFailureKind::GovernorOverload => 1,
                    PostClaimFailureKind::AcceptedResolution => 2,
                },
                "authoritative owner/reply acquisition must precede the failing selected-sidecar resolution hook"
            );
        }

        let records = engine.store().admissions(operation_id).await.unwrap();
        assert!(
            !records.is_empty() || matches!(fault, CleanupFault::DeleteAbort),
            "the post-Claimed path must expose its durable rows before the failing resolution hook"
        );
        match fault {
            CleanupFault::Healthy => {
                assert_eq!(records.len(), usize::try_from(cardinality).unwrap());
                assert!(records.iter().all(|record| {
                    record.state == hya_store::AdmissionState::Aborted && record.state.is_terminal()
                }));
            }
            CleanupFault::RejectAbort => {
                assert_eq!(records.len(), usize::try_from(cardinality).unwrap());
                assert!(
                    records
                        .iter()
                        .all(|record| record.state == hya_store::AdmissionState::Accepted),
                    "rejected cleanup must leave the claimed rows nonterminal"
                );
            }
            CleanupFault::DeleteAbort => {
                assert!(
                    records.is_empty(),
                    "missing-row cleanup proof must not be treated as durable terminal proof"
                );
            }
        };
        let expected_budget = match (kind, fault) {
            (PostClaimFailureKind::GovernorOverload, _) => 2_u64,
            (PostClaimFailureKind::AcceptedResolution, CleanupFault::Healthy) => {
                u64::from(cardinality)
            }
            (PostClaimFailureKind::AcceptedResolution, CleanupFault::RejectAbort)
            | (PostClaimFailureKind::AcceptedResolution, CleanupFault::DeleteAbort) => 0,
        };
        let remaining_budget = engine.governor().unwrap().remaining_budget(parent);
        let reply_pending = if matches!(fault, CleanupFault::Healthy) {
            false
        } else {
            matches!(
                reply_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            )
        };
        let contract_ok = cleanup_observed
            && match fault {
                CleanupFault::Healthy => !reply_pending,
                CleanupFault::RejectAbort | CleanupFault::DeleteAbort => reply_pending,
            }
            && remaining_budget == expected_budget;
        assert!(
            contract_ok,
            "post-Claimed cleanup contract failed: reply_pending={reply_pending}, remaining_budget={remaining_budget}, expected_budget={expected_budget}"
        );
        if let Some(preparation_task) = preparation_task {
            preparation_task.abort();
            let _ = preparation_task.await;
        }
        assert_eq!(
            engine.store().list_sessions().await.unwrap().len(),
            1,
            "post-Claimed failure must not allocate member sessions"
        );
        assert!(
            engine.store().active_actor_ids().await.unwrap().is_empty(),
            "post-Claimed failure must leave no active member actors"
        );
    }

    #[tokio::test]
    async fn post_claim_failures_use_owned_reply_after_proven_cleanup() {
        for kind in [
            PostClaimFailureKind::GovernorOverload,
            PostClaimFailureKind::AcceptedResolution,
        ] {
            post_claim_failure_case(kind, CleanupFault::Healthy, false).await;
        }
    }

    #[tokio::test]
    async fn post_claim_failures_acquire_owner_before_failing_resolution_hook() {
        let mut failures = Vec::new();
        for kind in [
            PostClaimFailureKind::GovernorOverload,
            PostClaimFailureKind::AcceptedResolution,
        ] {
            let label = format!("{kind:?}/Healthy");
            if tokio::spawn(post_claim_failure_case(kind, CleanupFault::Healthy, true))
                .await
                .is_err()
            {
                failures.push(label);
            }
        }
        assert!(
            failures.is_empty(),
            "each owner-before-hook case must satisfy its ordered observer contract; failures: {failures:?}"
        );
    }

    #[tokio::test]
    async fn post_claim_failures_keep_reply_pending_when_cleanup_is_unproven() {
        let mut failures = Vec::new();
        for kind in [
            PostClaimFailureKind::GovernorOverload,
            PostClaimFailureKind::AcceptedResolution,
        ] {
            for fault in [CleanupFault::RejectAbort, CleanupFault::DeleteAbort] {
                let label = format!("{kind:?}/{fault:?}");
                if tokio::spawn(post_claim_failure_case(kind, fault, false))
                    .await
                    .is_err()
                {
                    failures.push(label);
                }
            }
        }
        assert!(
            failures.is_empty(),
            "each post-Claimed cleanup-fault matrix case must satisfy the owner contract; failures: {failures:?}"
        );
    }

    #[tokio::test]
    async fn foreground_handler_uniform_pre_admission() {
        #[derive(Debug)]
        struct BarrierSample {
            name: &'static str,
            handler_active: usize,
            handler_owned: usize,
            reply_owners: usize,
            detached_spawns: usize,
            member_installations: usize,
        }

        let cases = [
            (
                "accepted",
                0_u32,
                1_usize,
                vec![hya_store::AdmissionState::Accepted],
            ),
            (
                "mixed",
                99_u32,
                2_usize,
                vec![
                    hya_store::AdmissionState::Accepted,
                    hya_store::AdmissionState::Queued,
                ],
            ),
            (
                "all-queued",
                100_u32,
                1_usize,
                vec![hya_store::AdmissionState::Queued],
            ),
        ];
        let mut observations = Vec::new();

        for (case_name, filler_count, member_count, expected_states) in cases {
            let workdir = tempdir();
            let provider_calls = Arc::new(AtomicUsize::new(0));
            let router = Arc::new(ProviderRouter::new().with(Arc::new(CountingDevProvider {
                calls: Arc::clone(&provider_calls),
                inner: DevProvider::new(),
                gate: None,
            })));
            let runtime = Arc::new(RuntimeRegistry::from_snapshot(
                ToolRegistry::builtins().snapshot(),
                builtin_agent_catalog().unwrap(),
            ));
            let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
            let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(8);
            let engine = Arc::new(
                SessionEngine::new(
                    SessionStore::connect_memory().await.unwrap(),
                    Arc::clone(&router),
                    runtime,
                    permission,
                    EventBus::default(),
                )
                .with_spawn_sender(spawn_sender.clone()),
            );
            let base = AgentSpec {
                name: AgentName::new("build"),
                model: ModelRef::new("dev"),
                system_prompt: "uniform pre-admission base".to_string(),
                workdir: workdir.clone(),
                reasoning: None,
            };
            let parent = engine
                .create(CreateSession {
                    parent: None,
                    agent: base.name.clone(),
                    model: base.model.clone(),
                    workdir: workdir.to_string_lossy().into_owned(),
                })
                .await
                .unwrap();
            let binding = engine.bind_runtime(&workdir).unwrap();
            let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();

            if filler_count > 0 {
                let filler_operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
                let filler_intents = (0..filler_count)
                    .map(|ordinal| AdmissionIntent {
                        runtime_fingerprint_version: 1,
                        runtime_fingerprint: [0x11; 32],
                        admission_binding_fingerprint_version: 1,
                        admission_binding_fingerprint: [0x22; 32],
                        spawn_intent: vec![0x33, u8::try_from(ordinal).unwrap()],
                    })
                    .collect();
                let filler_claim = AdmissionClaim {
                    operation_id: filler_operation.operation_id(),
                    source_tool_call_id: filler_operation.source_tool_call_id(),
                    root_session: parent,
                    request_fingerprint: [0x44; 32],
                    admission_units: filler_count,
                    actor_claim: None,
                };
                let filler_launches = engine
                    .store()
                    .claim_admission_batch(&filler_claim, filler_intents)
                    .await
                    .unwrap();
                match filler_launches {
                    AdmissionBatchClaimOutcome::Claimed(launches) => {
                        assert_eq!(launches.len(), filler_count as usize);
                    }
                    AdmissionBatchClaimOutcome::Existing => {
                        panic!("fresh filler admission must not already exist");
                    }
                }
            }

            let probe = Arc::new(ForegroundHandlerUniformProbe::new());
            let sidecar_environment = Arc::new(BundleSidecarEnvironment {
                command: None,
                staging_root: tempdir(),
                terminate_notify: None,
                test_observer: None,
                uniform_probe: Some(Arc::clone(&probe)),
            });
            let resident_supervisor = ResidentSupervisor::start(Arc::clone(&engine));
            let _spawn_supervisor = spawn_team_supervisor_with_environment(
                spawn_rx,
                Arc::clone(&engine),
                base,
                Arc::clone(&router),
                Arc::new(CategoryRegistry::default()),
                Arc::clone(&resident_supervisor),
                sidecar_environment,
            );

            let cancel = CancellationToken::new();
            let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
            let operation_id = operation.operation_id();
            let members = (0..member_count)
                .map(|ordinal| SpawnMember {
                    description: format!("uniform member {ordinal}"),
                    prompt: format!("uniform prompt {ordinal}"),
                    subagent_type: "general".to_string(),
                    ..SpawnMember::default()
                })
                .collect();
            let spawner = spawn_sender
                .for_binding(&binding)
                .for_session_with_agents(parent, agents);
            let request_cancel = cancel.clone();
            let request_task =
                tokio::spawn(
                    async move { spawner.spawn(operation, members, request_cancel).await },
                );

            let mut samples = Vec::new();
            let sample = |name: &'static str| BarrierSample {
                name,
                handler_active: probe.supervisor_handler_active.load(Ordering::SeqCst),
                handler_owned: probe.supervisor_handler_owned.load(Ordering::SeqCst),
                reply_owners: probe.reply_owners.load(Ordering::SeqCst),
                detached_spawns: probe.detached_postclaim_owner_spawns.load(Ordering::SeqCst),
                member_installations: probe.real_member_task_installations.load(Ordering::SeqCst),
            };

            let prepare_entered = probe.prepare_entered.notified();
            tokio::time::timeout(Duration::from_secs(5), prepare_entered)
                .await
                .expect("foreground preparation did not enter");
            samples.push(sample("prepare"));
            let before_claim = probe.before_claim.notified();
            probe.prepare_release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), before_claim)
                .await
                .expect("foreground preparation did not reach before-claim barrier");
            samples.push(sample("before-claim"));
            let after_claim = probe.after_claim.notified();
            probe.before_claim_release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), after_claim)
                .await
                .expect("foreground preparation did not reach after-claim barrier");
            let records = engine.store().admissions(operation_id).await.unwrap();
            let admission_counts = engine.store().admission_counts().await.unwrap();
            let states = records
                .iter()
                .map(|record| record.state)
                .collect::<Vec<_>>();
            samples.push(sample("post-claim"));
            let member_installations_at_claim =
                probe.real_member_task_installations.load(Ordering::SeqCst);
            let owner_entered = probe.owner_run_entered.notified();
            probe.after_claim_release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), owner_entered)
                .await
                .expect("inline foreground request handler owner did not enter");
            samples.push(sample("owner-entry"));

            let sessions = engine.store().list_sessions().await.unwrap();
            let active_actors = engine.store().active_actor_ids().await.unwrap();
            let reply_pending = !request_task.is_finished();
            let member_installations = probe.real_member_task_installations.load(Ordering::SeqCst);
            let provider_call_count = provider_calls.load(Ordering::SeqCst);
            let expected_active = filler_count
                + u32::try_from(
                    expected_states
                        .iter()
                        .filter(|state| **state == hya_store::AdmissionState::Accepted)
                        .count(),
                )
                .unwrap();
            let expected_non_active = u32::try_from(
                expected_states
                    .iter()
                    .filter(|state| **state == hya_store::AdmissionState::Queued)
                    .count(),
            )
            .unwrap();
            let counts_ok = admission_counts
                == hya_store::AdmissionCounts {
                    active: expected_active,
                    non_active: expected_non_active,
                    total: expected_active + expected_non_active,
                };
            let handler_delta = samples.last().map_or(0, |last| {
                last.handler_active
                    .saturating_sub(samples[0].handler_active)
            });
            let member_delta = member_installations.saturating_sub(member_installations_at_claim);
            let samples_ok = samples.iter().all(|sample| {
                !sample.name.is_empty()
                    && sample.handler_active == 1
                    && sample.handler_owned == 1
                    && sample.reply_owners == 1
                    && sample.detached_spawns == 0
                    && sample.member_installations == 0
            });
            let all_queued_ok = case_name != "all-queued"
                || (states
                    .iter()
                    .all(|state| *state == hya_store::AdmissionState::Queued)
                    && sessions.len() == 1
                    && active_actors.is_empty()
                    && resident_supervisor.team_cancel(parent).is_none()
                    && provider_call_count == 0
                    && member_installations == 0
                    && reply_pending);
            let case_ok = states == expected_states
                && counts_ok
                && samples_ok
                && handler_delta == 0
                && member_delta == 0
                && reply_pending
                && all_queued_ok;
            let barrier_names = samples.iter().map(|sample| sample.name).collect::<Vec<_>>();
            observations.push(format!(
                "{case_name}: ok={case_ok}, states={states:?}, samples={samples:?}, \
                 barriers={barrier_names:?}, \
                 handler_delta={handler_delta}, member_delta={member_delta}, \
                 admission_counts={admission_counts:?}, \
                 sessions={}, active_actors={}, provider_calls={provider_call_count}, \
                 member_installations={member_installations}, reply_pending={reply_pending}",
                sessions.len(),
                active_actors.len(),
            ));

            cancel.cancel();
            probe.owner_run_release.notify_one();
            request_task.abort();
            let _ = request_task.await;
        }

        assert!(
            observations
                .iter()
                .all(|observation| observation.contains("ok=true")),
            "foreground_handler_uniform_pre_admission ownership defect:\n{}",
            observations.join("\n")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn foreground_handler_cap_256() {
        let workdir = tempdir();
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let router = Arc::new(ProviderRouter::new().with(Arc::new(CountingDevProvider {
            calls: Arc::clone(&provider_calls),
            inner: DevProvider::new(),
            gate: None,
        })));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
        let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(1);
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "foreground handler cap base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();
        let probe = Arc::new(ForegroundHandlerUniformProbe::new());
        let sidecar_environment = Arc::new(BundleSidecarEnvironment {
            command: None,
            staging_root: tempdir(),
            terminate_notify: None,
            test_observer: None,
            uniform_probe: Some(Arc::clone(&probe)),
        });
        let resident_supervisor = ResidentSupervisor::start(Arc::clone(&engine));
        let _spawn_supervisor = spawn_team_supervisor_with_environment(
            spawn_rx,
            Arc::clone(&engine),
            base,
            Arc::clone(&router),
            Arc::new(CategoryRegistry::default()),
            Arc::clone(&resident_supervisor),
            sidecar_environment,
        );

        let spawner = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents);
        let unsupported_member = |ordinal: usize| SpawnMember {
            description: format!("cap member {ordinal}"),
            prompt: format!("cap prompt {ordinal}"),
            subagent_type: "general".to_string(),
            inline_agent: Some(InlineAgent {
                name: "unsupported".to_string(),
                prompt: "unsupported".to_string(),
                description: Some("unsupported inline description".to_string()),
                ..InlineAgent::default()
            }),
            ..SpawnMember::default()
        };

        use std::future::Future as _;
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        let mut parked_requests = Vec::with_capacity(256);
        for ordinal in 0..256 {
            let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
            let request_spawner = spawner.clone();
            let member = unsupported_member(ordinal);
            let mut request = Box::pin(async move {
                request_spawner
                    .spawn(operation, vec![member], CancellationToken::new())
                    .await
            });
            assert!(matches!(
                request.as_mut().poll(&mut context),
                std::task::Poll::Pending
            ));
            tokio::time::timeout(Duration::from_secs(5), probe.handler_acquisitions.acquire())
                .await
                .expect("foreground handler did not acquire its ownership probe")
                .expect("foreground handler acquisition probe closed")
                .forget();
            tokio::time::timeout(
                Duration::from_secs(5),
                probe.preparation_acquisitions.acquire(),
            )
            .await
            .expect("foreground preparation did not enter its ownership probe")
            .expect("foreground preparation acquisition probe closed")
            .forget();
            parked_requests.push(request);
        }

        assert_eq!(
            probe.supervisor_handler_active.load(Ordering::SeqCst),
            256,
            "all 256 foreground handlers must remain live before the overflow request"
        );
        assert_eq!(
            probe.supervisor_handler_owned.load(Ordering::SeqCst),
            256,
            "all 256 foreground handlers must retain their owner guard"
        );
        assert_eq!(
            probe.reply_owners.load(Ordering::SeqCst),
            256,
            "all 256 foreground handlers must retain their reply owner"
        );
        assert_eq!(
            probe.max_handler_live.load(Ordering::SeqCst),
            256,
            "the parked baseline must not exceed 256 live handlers"
        );
        assert_eq!(
            probe.preparation_entries.load(Ordering::SeqCst),
            256,
            "all 256 handlers must be parked before preparation"
        );
        assert_eq!(
            engine.store().admission_counts().await.unwrap(),
            hya_store::AdmissionCounts {
                active: 0,
                non_active: 0,
                total: 0,
            }
        );
        assert_eq!(
            engine.store().list_sessions().await.unwrap().len(),
            1,
            "preparation-only handlers must not allocate child sessions"
        );
        assert!(
            engine.store().active_actor_ids().await.unwrap().is_empty(),
            "preparation-only handlers must not install resident actors"
        );
        assert_eq!(
            provider_calls.load(Ordering::SeqCst),
            0,
            "preparation-only handlers must not invoke a provider"
        );
        assert_eq!(
            probe.real_member_task_installations.load(Ordering::SeqCst),
            0,
            "preparation-only handlers must not install member work"
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            probe.supervisor_full_observed.acquire(),
        )
        .await
        .expect("supervisor did not arm its full-cap intake branch")
        .expect("supervisor full-cap observation probe closed")
        .forget();

        let operation_257 = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_257_id = operation_257.operation_id();
        probe.watch_preparation(operation_257_id);
        let request_spawner_257 = spawner.clone();
        let member_257 = unsupported_member(257);
        let mut request_257 = Box::pin(async move {
            request_spawner_257
                .spawn(operation_257, vec![member_257], CancellationToken::new())
                .await
        });
        assert!(matches!(
            request_257.as_mut().poll(&mut context),
            std::task::Poll::Pending
        ));

        let request_spawner_258 = spawner.clone();
        let member_258 = unsupported_member(258);
        let operation_258 = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let mut request_258 = Box::pin(async move {
            request_spawner_258
                .spawn(operation_258, vec![member_258], CancellationToken::new())
                .await
        });
        assert!(matches!(
            request_258.as_mut().poll(&mut context),
            std::task::Poll::Ready(Err(SpawnError::Overloaded))
        ));

        tokio::task::yield_now().await;
        let target_entered_before_release = tokio::time::timeout(
            Duration::from_secs(1),
            probe.watched_preparation_entered.notified(),
        )
        .await
        .is_ok();
        let full_cap_snapshot = (
            probe.preparation_entries.load(Ordering::SeqCst),
            probe.supervisor_handler_active.load(Ordering::SeqCst),
            probe.supervisor_handler_owned.load(Ordering::SeqCst),
            probe.reply_owners.load(Ordering::SeqCst),
            probe.max_handler_live.load(Ordering::SeqCst),
        );
        assert_eq!(
            (target_entered_before_release, full_cap_snapshot),
            (false, (256, 256, 256, 256, 256)),
            "foreground_handler_cap_256 intake defect before release: target_entered={}, preparation_entries={}, live={}, owned={}, reply_owners={}, max={}",
            target_entered_before_release,
            full_cap_snapshot.0,
            full_cap_snapshot.1,
            full_cap_snapshot.2,
            full_cap_snapshot.3,
            full_cap_snapshot.4,
        );

        probe.prepare_release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), probe.handler_releases.acquire())
            .await
            .expect("one parked foreground handler did not release its ownership probe")
            .expect("foreground handler release probe closed")
            .forget();
        tokio::time::timeout(
            Duration::from_secs(5),
            probe.watched_preparation_entered.notified(),
        )
        .await
        .expect("request 257 did not enter foreground preparation after one slot released");
        assert_eq!(
            probe.preparation_entries.load(Ordering::SeqCst),
            257,
            "request 257 must enter preparation after one bounded-intake slot releases"
        );
        assert_eq!(
            probe.supervisor_handler_active.load(Ordering::SeqCst),
            256,
            "one released handler must make room for request 257"
        );
        assert_eq!(
            probe.max_handler_live.load(Ordering::SeqCst),
            256,
            "a bounded supervisor must never exceed the 256-handler peak"
        );
        probe.watched_preparation_release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), probe.handler_releases.acquire())
            .await
            .expect("request 257 handler did not release its ownership probe")
            .expect("foreground handler release probe closed")
            .forget();
        let request_257_result = tokio::time::timeout(
            Duration::from_secs(5),
            std::future::poll_fn(|cx| request_257.as_mut().poll(cx)),
        )
        .await
        .expect("request 257 did not return after its preparation gate released");
        assert!(matches!(
            request_257_result,
            Err(SpawnError::UnsupportedInlineAgentField {
                field: "description"
            })
        ));
        assert!(
            engine
                .store()
                .admissions(operation_257_id)
                .await
                .unwrap()
                .is_empty(),
            "request 257's unsupported inline field must fail before admission claim"
        );
        assert_eq!(
            engine.store().admission_counts().await.unwrap(),
            hya_store::AdmissionCounts {
                active: 0,
                non_active: 0,
                total: 0,
            }
        );
        assert_eq!(
            engine.store().list_sessions().await.unwrap().len(),
            1,
            "request 257 must not allocate a child session"
        );
        assert!(
            engine.store().active_actor_ids().await.unwrap().is_empty(),
            "request 257 must not install a resident actor"
        );
        assert_eq!(
            provider_calls.load(Ordering::SeqCst),
            0,
            "request 257 must not invoke a provider"
        );
        assert_eq!(
            probe.real_member_task_installations.load(Ordering::SeqCst),
            0,
            "request 257 must not install member work"
        );
        for _ in 0..255 {
            probe.prepare_release.notify_one();
            tokio::time::timeout(Duration::from_secs(5), probe.handler_releases.acquire())
                .await
                .expect("parked foreground handler did not release its ownership probe")
                .expect("foreground handler release probe closed")
                .forget();
        }
        for mut request in parked_requests {
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                std::future::poll_fn(|cx| request.as_mut().poll(cx)),
            )
            .await
            .expect("parked request did not finish after its preparation gate released");
            assert!(matches!(
                result,
                Err(SpawnError::UnsupportedInlineAgentField {
                    field: "description"
                })
            ));
        }

        assert_eq!(
            probe.supervisor_handler_active.load(Ordering::SeqCst),
            0,
            "all parked foreground handlers must release during test cleanup"
        );
    }

    #[tokio::test]
    async fn foreign_promotion_is_wake_only() {
        let workdir = tempdir().join("foreign-promotion-wake-only-workdir");
        std::fs::create_dir_all(&workdir).unwrap();
        let database = tempdir().join("foreign-promotion-wake-only.db");
        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();

        let prepared = ["worker-a", "worker-b"]
            .into_iter()
            .map(|worker| {
                prepare_package(BundleSource::new(
                    format!("foreign-promotion-wake-only-{worker}"),
                    vec![
                        SourceFile::new(
                            "bundle.yaml",
                            format!(
                                "kind: AgentBundle\nidentity:\n  id: hya/foreign-promotion-wake-only-{worker}\n  version: 0.0.1\n  publisher: hya-tests\nagent:\n  id: {worker}\n  role: subagent\n  spawn_lifecycle: transient\n  prompt: prompts/agent.md\n"
                            )
                            .into_bytes(),
                        ),
                        SourceFile::new(
                            "prompts/agent.md",
                            format!("foreign wake {worker} prompt").into_bytes(),
                        ),
                    ],
                ))
                .expect("foreign-promotion catalog must prepare")
            })
            .collect::<Vec<_>>();
        let prepared_refs = prepared.iter().collect::<Vec<_>>();
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_verified_catalogs(&prepared_refs)
                .expect("%s catalog must retain verified identity"),
        ));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            catalog,
        ));

        let provider_calls = Arc::new(AtomicUsize::new(0));
        let provider_gate = Arc::new(ProviderGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let router = Arc::new(ProviderRouter::new().with(Arc::new(CountingDevProvider {
            calls: Arc::clone(&provider_calls),
            inner: DevProvider::new(),
            gate: Some(Arc::clone(&provider_gate)),
        })));
        let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
        let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(8);
        let engine = Arc::new(
            SessionEngine::new(
                store.clone(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "foreign wake base prompt".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent_a = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let parent_b = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();

        let filler_operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let filler_intents = (0..99)
            .map(|ordinal| AdmissionIntent {
                runtime_fingerprint_version: 1,
                runtime_fingerprint: [0x11; 32],
                admission_binding_fingerprint_version: 1,
                admission_binding_fingerprint: [0x22; 32],
                spawn_intent: vec![0x33, u8::try_from(ordinal).unwrap()],
            })
            .collect();
        let filler_claim = AdmissionClaim {
            operation_id: filler_operation.operation_id(),
            source_tool_call_id: filler_operation.source_tool_call_id(),
            root_session: parent_a,
            request_fingerprint: [0x44; 32],
            admission_units: 99,
            actor_claim: None,
        };
        let filler_launches = store
            .claim_admission_batch(&filler_claim, filler_intents)
            .await
            .unwrap();
        match filler_launches {
            AdmissionBatchClaimOutcome::Claimed(launches) => assert_eq!(launches.len(), 99),
            AdmissionBatchClaimOutcome::Existing => {
                panic!("fresh filler admission must not already exist")
            }
        }

        let observer = Arc::new(AdmissionTestObserver::new());
        let sidecar_environment = Arc::new(BundleSidecarEnvironment {
            command: None,
            staging_root: tempdir(),
            terminate_notify: None,
            test_observer: Some(Arc::clone(&observer)),
            uniform_probe: None,
        });
        let resident_supervisor = ResidentSupervisor::start(Arc::clone(&engine));
        let _spawn_supervisor = spawn_team_supervisor_with_environment(
            spawn_rx,
            Arc::clone(&engine),
            base,
            Arc::clone(&router),
            Arc::new(CategoryRegistry::default()),
            Arc::clone(&resident_supervisor),
            sidecar_environment,
        );

        let operation_a = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_a_id = operation_a.operation_id();
        let cancel_a = CancellationToken::new();
        let members_a = vec![
            SpawnMember {
                description: "foreign wake A0".to_string(),
                prompt: "foreign wake A0".to_string(),
                subagent_type: "worker-a".to_string(),
                ..SpawnMember::default()
            },
            SpawnMember {
                description: "foreign wake A1".to_string(),
                prompt: "foreign wake A1".to_string(),
                subagent_type: "worker-a".to_string(),
                ..SpawnMember::default()
            },
        ];
        let spawner_a = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent_a, agents.clone());
        let cancel_a_task = cancel_a.clone();
        let request_a =
            tokio::spawn(
                async move { spawner_a.spawn(operation_a, members_a, cancel_a_task).await },
            );

        tokio::time::timeout(Duration::from_secs(5), provider_gate.entered.notified())
            .await
            .expect("A0 provider did not enter its gate");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let records = store.admissions(operation_a_id).await.unwrap();
                if records.len() == 2
                    && records[0].state == hya_store::AdmissionState::Started
                    && records[1].state == hya_store::AdmissionState::Queued
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("A0 must be started while A1 remains queued");

        let mut corrupt_connection =
            SqliteConnection::connect(&format!("sqlite://{}", database.display()))
                .await
                .unwrap();
        sqlx::query(
            "UPDATE admission_journal SET admission_binding_fingerprint = ? \
             WHERE operation_id = ? AND member_ordinal = 1",
        )
        .bind(vec![0x99_u8; 32])
        .bind(operation_a_id.as_uuid().as_bytes().as_slice())
        .execute(&mut corrupt_connection)
        .await
        .unwrap();
        corrupt_connection.close().await.unwrap();

        let operation_b = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_b_id = operation_b.operation_id();
        let cancel_b = CancellationToken::new();
        let spawner_b = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent_b, agents);
        let cancel_b_task = cancel_b.clone();
        let request_b = tokio::spawn(async move {
            spawner_b
                .spawn(
                    operation_b,
                    vec![SpawnMember {
                        description: "foreign wake B0".to_string(),
                        prompt: "foreign wake B0".to_string(),
                        subagent_type: "worker-b".to_string(),
                        ..SpawnMember::default()
                    }],
                    cancel_b_task,
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let b_owner_acquired = {
                    let owners = observer
                        .owner_operation_ids
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    owners.contains(&operation_b_id)
                };
                if b_owner_acquired {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("B foreground owner did not acquire its request");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let records = store.admissions(operation_b_id).await.unwrap();
                if records.len() == 1 && records[0].state == hya_store::AdmissionState::Queued {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("B must remain queued before A0 is released");

        let baseline_targets = observer
            .resolution_targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert!(
            baseline_targets.iter().any(|target| target == "worker-a"),
            "A0's initial launch must be resolved before the promotion barrier"
        );

        let mut blocker = SqliteConnection::connect(&format!("sqlite://{}", database.display()))
            .await
            .unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut blocker)
            .await
            .unwrap();
        let foreign_wake = observer.foreign_wake.notified();
        tokio::pin!(foreign_wake);
        provider_gate.release.notify_one();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let b_queued_while_locked = store
            .admission(operation_b_id)
            .await
            .unwrap()
            .is_some_and(|record| record.state == hya_store::AdmissionState::Queued);
        let foreign_wake_while_locked =
            tokio::time::timeout(Duration::from_millis(100), &mut foreign_wake)
                .await
                .is_ok();
        sqlx::query("COMMIT").execute(&mut blocker).await.unwrap();
        blocker.close().await.unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let record = store
                    .admission(operation_b_id)
                    .await
                    .unwrap()
                    .expect("B admission row");
                if record.state == hya_store::AdmissionState::Accepted {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("A finalize must commit B's promotion");
        let foreign_wake_after_commit =
            tokio::time::timeout(Duration::from_secs(1), &mut foreign_wake)
                .await
                .is_ok();
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let promoted_target_observed = {
                    let targets = observer
                        .resolution_targets
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    targets.len() > baseline_targets.len()
                };
                if promoted_target_observed {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        let _ =
            tokio::time::timeout(Duration::from_secs(1), provider_gate.entered.notified()).await;

        let resolution_targets = observer
            .resolution_targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let promoted_targets = resolution_targets[baseline_targets.len()..].to_vec();
        let foreign_wake_operation_ids = observer
            .foreign_wake_operation_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let b_record = store
            .admission(operation_b_id)
            .await
            .unwrap()
            .expect("B admission row must remain readable");
        // A's path must only wake B after durable commit. B's owner may then
        // rehydrate (see owner_rehydrates_accepted_ordinals_exactly_once_on_wake).
        assert_eq!(
            (
                foreign_wake_while_locked,
                foreign_wake_after_commit,
                foreign_wake_operation_ids,
                b_queued_while_locked,
            ),
            (false, true, vec![operation_b_id], true),
            "foreign promotion must wake B only after A's commit"
        );
        assert!(
            !baseline_targets.iter().any(|target| target == "worker-b"),
            "A must not resolve B before the foreign wake"
        );
        assert!(
            matches!(
                b_record.state,
                hya_store::AdmissionState::Accepted | hya_store::AdmissionState::Started
            ) || b_record.state.is_terminal(),
            "B must be promoted after A's commit; state={:?}",
            b_record.state
        );
        let _ = (
            promoted_targets,
            provider_calls.load(Ordering::SeqCst),
            engine.store().list_sessions().await.unwrap().len(),
        );

        cancel_a.cancel();
        cancel_b.cancel();
        provider_gate.release.notify_waiters();
        request_a.abort();
        request_b.abort();
        let _ = request_a.await;
        let _ = request_b.await;
    }

    #[tokio::test]
    async fn owner_rehydrates_accepted_ordinals_exactly_once_on_wake() {
        // Derivative of foreign_promotion_is_wake_only: after the foreign wake that
        // leaves B Accepted, B's owner must rehydrate and start that ordinal once.
        let workdir = tempdir().join("owner-rehydrate-wake-workdir");
        std::fs::create_dir_all(&workdir).unwrap();
        let database = tempdir().join("owner-rehydrate-wake.db");
        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();

        let prepared = ["worker-a", "worker-b"]
            .into_iter()
            .map(|worker| {
                prepare_package(BundleSource::new(
                    format!("owner-rehydrate-wake-{worker}"),
                    vec![
                        SourceFile::new(
                            "bundle.yaml",
                            format!(
                                "kind: AgentBundle\nidentity:\n  id: hya/owner-rehydrate-wake-{worker}\n  version: 0.0.1\n  publisher: hya-tests\nagent:\n  id: {worker}\n  role: subagent\n  spawn_lifecycle: transient\n  prompt: prompts/agent.md\n"
                            )
                            .into_bytes(),
                        ),
                        SourceFile::new(
                            "prompts/agent.md",
                            format!("owner rehydrate {worker} prompt").into_bytes(),
                        ),
                    ],
                ))
                .expect("owner-rehydrate catalog must prepare")
            })
            .collect::<Vec<_>>();
        let prepared_refs = prepared.iter().collect::<Vec<_>>();
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_verified_catalogs(&prepared_refs)
                .expect("%s catalog must retain verified identity"),
        ));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            catalog,
        ));

        let provider_calls = Arc::new(AtomicUsize::new(0));
        let provider_gate = Arc::new(ProviderGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let router = Arc::new(ProviderRouter::new().with(Arc::new(CountingDevProvider {
            calls: Arc::clone(&provider_calls),
            inner: DevProvider::new(),
            gate: Some(Arc::clone(&provider_gate)),
        })));
        let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
        let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(8);
        let engine = Arc::new(
            SessionEngine::new(
                store.clone(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "owner rehydrate base prompt".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent_a = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let parent_b = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();

        let filler_operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let filler_intents = (0..99)
            .map(|ordinal| AdmissionIntent {
                runtime_fingerprint_version: 1,
                runtime_fingerprint: [0x11; 32],
                admission_binding_fingerprint_version: 1,
                admission_binding_fingerprint: [0x22; 32],
                spawn_intent: vec![0x33, u8::try_from(ordinal).unwrap()],
            })
            .collect();
        let filler_claim = AdmissionClaim {
            operation_id: filler_operation.operation_id(),
            source_tool_call_id: filler_operation.source_tool_call_id(),
            root_session: parent_a,
            request_fingerprint: [0x44; 32],
            admission_units: 99,
            actor_claim: None,
        };
        let filler_launches = store
            .claim_admission_batch(&filler_claim, filler_intents)
            .await
            .unwrap();
        match filler_launches {
            AdmissionBatchClaimOutcome::Claimed(launches) => assert_eq!(launches.len(), 99),
            AdmissionBatchClaimOutcome::Existing => {
                panic!("fresh filler admission must not already exist")
            }
        }

        let observer = Arc::new(AdmissionTestObserver::new());
        let sidecar_environment = Arc::new(BundleSidecarEnvironment {
            command: None,
            staging_root: tempdir(),
            terminate_notify: None,
            test_observer: Some(Arc::clone(&observer)),
            uniform_probe: None,
        });
        let resident_supervisor = ResidentSupervisor::start(Arc::clone(&engine));
        let _spawn_supervisor = spawn_team_supervisor_with_environment(
            spawn_rx,
            Arc::clone(&engine),
            base,
            Arc::clone(&router),
            Arc::new(CategoryRegistry::default()),
            Arc::clone(&resident_supervisor),
            sidecar_environment,
        );

        let operation_a = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_a_id = operation_a.operation_id();
        let cancel_a = CancellationToken::new();
        let members_a = vec![
            SpawnMember {
                description: "owner rehydrate A0".to_string(),
                prompt: "owner rehydrate A0".to_string(),
                subagent_type: "worker-a".to_string(),
                ..SpawnMember::default()
            },
            SpawnMember {
                description: "owner rehydrate A1".to_string(),
                prompt: "owner rehydrate A1".to_string(),
                subagent_type: "worker-a".to_string(),
                ..SpawnMember::default()
            },
        ];
        let spawner_a = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent_a, agents.clone());
        let cancel_a_task = cancel_a.clone();
        let request_a =
            tokio::spawn(
                async move { spawner_a.spawn(operation_a, members_a, cancel_a_task).await },
            );

        tokio::time::timeout(Duration::from_secs(5), provider_gate.entered.notified())
            .await
            .expect("A0 provider did not enter its gate");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let records = store.admissions(operation_a_id).await.unwrap();
                if records.len() == 2
                    && records[0].state == hya_store::AdmissionState::Started
                    && records[1].state == hya_store::AdmissionState::Queued
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("A0 must be started while A1 remains queued");

        let mut corrupt_connection =
            SqliteConnection::connect(&format!("sqlite://{}", database.display()))
                .await
                .unwrap();
        sqlx::query(
            "UPDATE admission_journal SET admission_binding_fingerprint = ? \
             WHERE operation_id = ? AND member_ordinal = 1",
        )
        .bind(vec![0x99_u8; 32])
        .bind(operation_a_id.as_uuid().as_bytes().as_slice())
        .execute(&mut corrupt_connection)
        .await
        .unwrap();
        corrupt_connection.close().await.unwrap();

        let operation_b = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_b_id = operation_b.operation_id();
        let cancel_b = CancellationToken::new();
        let spawner_b = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent_b, agents);
        let cancel_b_task = cancel_b.clone();
        let request_b = tokio::spawn(async move {
            spawner_b
                .spawn(
                    operation_b,
                    vec![SpawnMember {
                        description: "owner rehydrate B0".to_string(),
                        prompt: "owner rehydrate B0".to_string(),
                        subagent_type: "worker-b".to_string(),
                        ..SpawnMember::default()
                    }],
                    cancel_b_task,
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let b_owner_acquired = {
                    let owners = observer
                        .owner_operation_ids
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    owners.contains(&operation_b_id)
                };
                if b_owner_acquired {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("B foreground owner did not acquire its request");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let records = store.admissions(operation_b_id).await.unwrap();
                if records.len() == 1 && records[0].state == hya_store::AdmissionState::Queued {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("B must remain queued before A0 is released");

        let baseline_targets = observer
            .resolution_targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert!(
            baseline_targets.iter().any(|target| target == "worker-a"),
            "A0's initial launch must be resolved before the promotion barrier"
        );

        let mut blocker = SqliteConnection::connect(&format!("sqlite://{}", database.display()))
            .await
            .unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut blocker)
            .await
            .unwrap();
        let foreign_wake = observer.foreign_wake.notified();
        tokio::pin!(foreign_wake);
        provider_gate.release.notify_one();
        tokio::time::sleep(Duration::from_millis(100)).await;
        sqlx::query("COMMIT").execute(&mut blocker).await.unwrap();
        blocker.close().await.unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let record = store
                    .admission(operation_b_id)
                    .await
                    .unwrap()
                    .expect("B admission row");
                if record.state == hya_store::AdmissionState::Accepted
                    || record.state == hya_store::AdmissionState::Started
                    || record.state.is_terminal()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("A finalize must commit B's promotion");
        assert!(
            tokio::time::timeout(Duration::from_secs(1), &mut foreign_wake)
                .await
                .is_ok(),
            "foreign promotion must wake B's owner"
        );

        // RED: without owner rehydrate this times out while B stays Accepted.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let record = store
                    .admission(operation_b_id)
                    .await
                    .unwrap()
                    .expect("B admission row");
                if record.state == hya_store::AdmissionState::Started || record.state.is_terminal()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("B owner must rehydrate Accepted ordinal and cross Started after wake");

        provider_gate.release.notify_waiters();
        tokio::time::sleep(Duration::from_millis(100)).await;

        let worker_b_resolutions = observer
            .resolution_targets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|target| *target == "worker-b")
            .count();
        let b_record = store
            .admission(operation_b_id)
            .await
            .unwrap()
            .expect("B admission row");
        assert_eq!(
            (
                b_record.state == hya_store::AdmissionState::Started
                    || b_record.state.is_terminal(),
                worker_b_resolutions,
            ),
            (true, 1),
            "B must start/terminalize with exactly one worker-b resolve"
        );

        cancel_a.cancel();
        cancel_b.cancel();
        provider_gate.release.notify_waiters();
        request_a.abort();
        request_b.abort();
        let _ = request_a.await;
        let _ = request_b.await;
    }

    #[tokio::test]
    async fn identical_members_preserve_exact_ordered_reply() {
        /// Completes the second concurrent stream before the first so completion
        /// order is reverse of ordinal order. Reply must still be ordinal-ordered.
        struct ReverseCompletionProvider {
            inner: DevProvider,
            calls: AtomicUsize,
            second_done: tokio::sync::Notify,
        }

        #[async_trait]
        impl Provider for ReverseCompletionProvider {
            fn id(&self) -> &str {
                self.inner.id()
            }

            fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
                self.inner.capabilities(model)
            }

            fn configured_identity_v1(&self) -> Option<Vec<u8>> {
                self.inner.configured_identity_v1()
            }

            async fn stream(
                &self,
                request: CompletionRequest,
                session: SessionId,
                message: hya_proto::MessageId,
            ) -> Result<EventStream, ProviderError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    self.second_done.notified().await;
                    self.inner.stream(request, session, message).await
                } else {
                    let result = self.inner.stream(request, session, message).await;
                    self.second_done.notify_waiters();
                    result
                }
            }
        }

        let workdir = tempdir();
        let reverse = Arc::new(ReverseCompletionProvider {
            inner: DevProvider::new(),
            calls: AtomicUsize::new(0),
            second_done: tokio::sync::Notify::new(),
        });
        let reverse_provider: Arc<dyn Provider> = reverse.clone();
        let router = Arc::new(ProviderRouter::new().with(reverse_provider));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
        let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(8);
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "identical ordered reply base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();
        let sidecar_environment = Arc::new(BundleSidecarEnvironment {
            command: None,
            staging_root: tempdir(),
            terminate_notify: None,
            test_observer: None,
            uniform_probe: None,
        });
        let resident_supervisor = ResidentSupervisor::start(Arc::clone(&engine));
        let _spawn_supervisor = spawn_team_supervisor_with_environment(
            spawn_rx,
            Arc::clone(&engine),
            base,
            Arc::clone(&router),
            Arc::new(CategoryRegistry::default()),
            Arc::clone(&resident_supervisor),
            sidecar_environment,
        );

        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let members = vec![
            SpawnMember {
                description: "identical-0".to_string(),
                prompt: "ORDINAL-MARKER-0".to_string(),
                subagent_type: "general".to_string(),
                ..SpawnMember::default()
            },
            SpawnMember {
                description: "identical-1".to_string(),
                prompt: "ORDINAL-MARKER-1".to_string(),
                subagent_type: "general".to_string(),
                ..SpawnMember::default()
            },
        ];
        let spawner = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents);
        let reply = tokio::time::timeout(
            Duration::from_secs(15),
            spawner.spawn(operation, members, CancellationToken::new()),
        )
        .await
        .expect("identical-member spawn timed out")
        .expect("identical-member spawn must succeed");

        assert_eq!(reply.len(), 2, "reply must contain one outcome per ordinal");
        assert_ne!(
            reply[0].member, reply[1].member,
            "identical members must still have distinct MemberId identities"
        );
        assert_ne!(
            reply[0].session, reply[1].session,
            "identical members must still have distinct sessions"
        );
        assert!(
            reply.iter().all(|outcome| outcome.status == "done"),
            "both members must complete successfully: {reply:?}"
        );
        // Ordinal order is authoritative even when completion was reverse.
        assert!(
            reply[0].summary.contains("ORDINAL-MARKER-0"),
            "outcomes[0] must be ordinal 0 evidence, got summary={}",
            reply[0].summary
        );
        assert!(
            reply[1].summary.contains("ORDINAL-MARKER-1"),
            "outcomes[1] must be ordinal 1 evidence, got summary={}",
            reply[1].summary
        );
        assert_eq!(
            reverse.calls.load(Ordering::SeqCst),
            2,
            "both members must have run model turns"
        );
        // Projection must not be required for ordered reply correctness.
        let projection = engine.read_projection(parent).await.unwrap();
        assert!(
            projection.session.members.len() >= 2,
            "projection may observe members, but reply identity is owner-local"
        );
    }

    #[tokio::test]
    async fn background_running_reply_only_after_registration() {
        // Consult30 RED 2: one-member background under full capacity stays Queued
        // with no reply; after promotion + Started + session registration, one
        // running outcome is returned.
        let workdir = tempdir();
        let provider_gate = Arc::new(ProviderGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let router = Arc::new(ProviderRouter::new().with(Arc::new(CountingDevProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            inner: DevProvider::new(),
            gate: Some(Arc::clone(&provider_gate)),
        })));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
        let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(8);
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "background delayed reply base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();

        let filler_operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let filler_intents = (0..99)
            .map(|ordinal| AdmissionIntent {
                runtime_fingerprint_version: 1,
                runtime_fingerprint: [0x11; 32],
                admission_binding_fingerprint_version: 1,
                admission_binding_fingerprint: [0x22; 32],
                spawn_intent: vec![0x33, u8::try_from(ordinal % 200).unwrap()],
            })
            .collect();
        let filler_claim = AdmissionClaim {
            operation_id: filler_operation.operation_id(),
            source_tool_call_id: filler_operation.source_tool_call_id(),
            root_session: parent,
            request_fingerprint: [0x55; 32],
            admission_units: 99,
            actor_claim: None,
        };
        match engine
            .store()
            .claim_admission_batch(&filler_claim, filler_intents)
            .await
            .unwrap()
        {
            AdmissionBatchClaimOutcome::Claimed(launches) => assert_eq!(launches.len(), 99),
            AdmissionBatchClaimOutcome::Existing => panic!("filler must be fresh"),
        }

        let _spawn_supervisor = spawn_team_supervisor_with_environment(
            spawn_rx,
            Arc::clone(&engine),
            base,
            Arc::clone(&router),
            Arc::new(CategoryRegistry::default()),
            ResidentSupervisor::start(Arc::clone(&engine)),
            Arc::new(BundleSidecarEnvironment {
                command: None,
                staging_root: tempdir(),
                terminate_notify: None,
                test_observer: None,
                uniform_probe: None,
            }),
        );

        let holder_op = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let holder_spawner = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents.clone());
        let holder_task = tokio::spawn(async move {
            holder_spawner
                .spawn(
                    holder_op,
                    vec![SpawnMember {
                        description: "holder".to_string(),
                        prompt: "HOLDER".to_string(),
                        subagent_type: "general".to_string(),
                        ..SpawnMember::default()
                    }],
                    CancellationToken::new(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), provider_gate.entered.notified())
            .await
            .expect("foreground holder must enter provider gate");

        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_id = operation.operation_id();
        let spawner = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents);
        let reply_task = tokio::spawn(async move {
            spawner
                .spawn_background(
                    operation,
                    vec![SpawnMember {
                        description: "bg-queued".to_string(),
                        prompt: "BACKGROUND-MARKER".to_string(),
                        subagent_type: "general".to_string(),
                        ..SpawnMember::default()
                    }],
                    CancellationToken::new(),
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let record = engine.store().admission(operation_id).await.unwrap();
                if record.is_some_and(|r| r.state == hya_store::AdmissionState::Queued) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background member must be durably Queued under full capacity");
        assert!(
            !reply_task.is_finished(),
            "background must not reply while Queued"
        );
        // parent + holder session only; background must not register yet.
        let sessions_while_queued = engine.store().list_sessions().await.unwrap().len();
        assert_eq!(
            sessions_while_queued, 2,
            "queued background must not allocate its own child session before registration"
        );

        provider_gate.release.notify_waiters();
        let _ = tokio::time::timeout(Duration::from_secs(10), holder_task).await;

        let reply = tokio::time::timeout(Duration::from_secs(15), reply_task)
            .await
            .expect("background reply timed out after promotion")
            .expect("join")
            .expect("background must reply Ok(running) after registration");
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0].status, "running");
        assert_ne!(reply[0].session, "-");
        assert_ne!(reply[0].session, "");
        let record = engine
            .store()
            .admission(operation_id)
            .await
            .unwrap()
            .expect("background admission row");
        assert!(
            record.state == hya_store::AdmissionState::Started || record.state.is_terminal(),
            "after registration reply, row must have crossed Started: {:?}",
            record.state
        );
        assert!(
            engine.store().list_sessions().await.unwrap().len() > sessions_while_queued,
            "registration must create a real child session for the background member"
        );
    }

    #[tokio::test]
    async fn mixed_foreground_cancel_waits_for_whole_batch_terminal() {
        // Consult30 RED 6: mixed Accepted/Queued foreground batch cancelled after
        // claim must durable-terminalize every member and complete the caller
        // with Cancelled (no partial success reply).
        let workdir = tempdir();
        let provider_gate = Arc::new(ProviderGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let router = Arc::new(ProviderRouter::new().with(Arc::new(CountingDevProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            inner: DevProvider::new(),
            gate: Some(Arc::clone(&provider_gate)),
        })));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
        let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(8);
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "mixed cancel base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();

        let filler_operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let filler_intents = (0..99)
            .map(|ordinal| AdmissionIntent {
                runtime_fingerprint_version: 1,
                runtime_fingerprint: [0x11; 32],
                admission_binding_fingerprint_version: 1,
                admission_binding_fingerprint: [0x22; 32],
                spawn_intent: vec![0x33, u8::try_from(ordinal % 200).unwrap()],
            })
            .collect();
        let filler_claim = AdmissionClaim {
            operation_id: filler_operation.operation_id(),
            source_tool_call_id: filler_operation.source_tool_call_id(),
            root_session: parent,
            request_fingerprint: [0x99; 32],
            admission_units: 99,
            actor_claim: None,
        };
        match engine
            .store()
            .claim_admission_batch(&filler_claim, filler_intents)
            .await
            .unwrap()
        {
            AdmissionBatchClaimOutcome::Claimed(launches) => assert_eq!(launches.len(), 99),
            AdmissionBatchClaimOutcome::Existing => panic!("filler must be fresh"),
        }

        let _spawn_supervisor = spawn_team_supervisor_with_environment(
            spawn_rx,
            Arc::clone(&engine),
            base,
            Arc::clone(&router),
            Arc::new(CategoryRegistry::default()),
            ResidentSupervisor::start(Arc::clone(&engine)),
            Arc::new(BundleSidecarEnvironment {
                command: None,
                staging_root: tempdir(),
                terminate_notify: None,
                test_observer: None,
                uniform_probe: None,
            }),
        );

        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_id = operation.operation_id();
        let cancel = CancellationToken::new();
        let spawner = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents);
        let cancel_task = cancel.clone();
        let reply_task = tokio::spawn(async move {
            spawner
                .spawn(
                    operation,
                    vec![
                        SpawnMember {
                            description: "mix-0".to_string(),
                            prompt: "MIX-0".to_string(),
                            subagent_type: "general".to_string(),
                            ..SpawnMember::default()
                        },
                        SpawnMember {
                            description: "mix-1".to_string(),
                            prompt: "MIX-1".to_string(),
                            subagent_type: "general".to_string(),
                            ..SpawnMember::default()
                        },
                    ],
                    cancel_task,
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), provider_gate.entered.notified())
            .await
            .expect("accepted member must enter gate");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let records = engine.store().admissions(operation_id).await.unwrap();
                if records.len() == 2
                    && records
                        .iter()
                        .any(|r| r.state == hya_store::AdmissionState::Started)
                    && records
                        .iter()
                        .any(|r| r.state == hya_store::AdmissionState::Queued)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("mixed Started+Queued batch");

        cancel.cancel();
        // Release the in-flight Started member so cancel/cleanup can converge.
        provider_gate.release.notify_waiters();

        let reply = tokio::time::timeout(Duration::from_secs(10), reply_task)
            .await
            .expect("mixed cancel reply timed out")
            .expect("join");
        assert!(
            matches!(reply, Err(SpawnError::Cancelled)),
            "mixed batch cancel must complete as Cancelled, got {reply:?}"
        );
        let records = engine.store().admissions(operation_id).await.unwrap();
        assert_eq!(records.len(), 2);
        assert!(
            records.iter().all(|r| r.state.is_terminal()),
            "every member must be durable-terminal: {records:?}"
        );
    }

    #[tokio::test]
    async fn dropped_receiver_converges_without_implicit_cancel() {
        // Consult30 RED 5: dropping the caller oneshot receiver must not panic,
        // implicitly cancel, or block durable promotion/registration.
        let workdir = tempdir();
        let provider_gate = Arc::new(ProviderGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let router = Arc::new(ProviderRouter::new().with(Arc::new(CountingDevProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            inner: DevProvider::new(),
            gate: Some(Arc::clone(&provider_gate)),
        })));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
        let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(8);
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "drop-receiver base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();

        let filler_operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let filler_intents = (0..99)
            .map(|ordinal| AdmissionIntent {
                runtime_fingerprint_version: 1,
                runtime_fingerprint: [0x11; 32],
                admission_binding_fingerprint_version: 1,
                admission_binding_fingerprint: [0x22; 32],
                spawn_intent: vec![0x33, u8::try_from(ordinal % 200).unwrap()],
            })
            .collect();
        let filler_claim = AdmissionClaim {
            operation_id: filler_operation.operation_id(),
            source_tool_call_id: filler_operation.source_tool_call_id(),
            root_session: parent,
            request_fingerprint: [0x88; 32],
            admission_units: 99,
            actor_claim: None,
        };
        match engine
            .store()
            .claim_admission_batch(&filler_claim, filler_intents)
            .await
            .unwrap()
        {
            AdmissionBatchClaimOutcome::Claimed(launches) => assert_eq!(launches.len(), 99),
            AdmissionBatchClaimOutcome::Existing => panic!("filler must be fresh"),
        }

        let _spawn_supervisor = spawn_team_supervisor_with_environment(
            spawn_rx,
            Arc::clone(&engine),
            base,
            Arc::clone(&router),
            Arc::new(CategoryRegistry::default()),
            ResidentSupervisor::start(Arc::clone(&engine)),
            Arc::new(BundleSidecarEnvironment {
                command: None,
                staging_root: tempdir(),
                terminate_notify: None,
                test_observer: None,
                uniform_probe: None,
            }),
        );

        let holder_op = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let holder_spawner = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents.clone());
        let holder_task = tokio::spawn(async move {
            holder_spawner
                .spawn(
                    holder_op,
                    vec![SpawnMember {
                        description: "holder".to_string(),
                        prompt: "HOLDER".to_string(),
                        subagent_type: "general".to_string(),
                        ..SpawnMember::default()
                    }],
                    CancellationToken::new(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), provider_gate.entered.notified())
            .await
            .expect("holder must enter gate");

        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_id = operation.operation_id();
        let spawner = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents);
        let reply_task = tokio::spawn(async move {
            spawner
                .spawn_background(
                    operation,
                    vec![SpawnMember {
                        description: "drop-rx".to_string(),
                        prompt: "DROP-RX".to_string(),
                        subagent_type: "general".to_string(),
                        ..SpawnMember::default()
                    }],
                    CancellationToken::new(),
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let record = engine.store().admission(operation_id).await.unwrap();
                if record.is_some_and(|r| r.state == hya_store::AdmissionState::Queued) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background must queue");

        // Drop the caller oneshot receiver without cancelling durable work.
        reply_task.abort();
        let _ = reply_task.await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let after_drop = engine
            .store()
            .admission(operation_id)
            .await
            .unwrap()
            .expect("row");
        assert_eq!(
            after_drop.state,
            hya_store::AdmissionState::Queued,
            "receiver drop must not implicitly cancel durable Queued work"
        );

        // Promote via holder completion; owner must converge without a reply owner.
        provider_gate.release.notify_waiters();
        let _ = tokio::time::timeout(Duration::from_secs(10), holder_task).await;

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let record = engine
                    .store()
                    .admission(operation_id)
                    .await
                    .unwrap()
                    .expect("row");
                if record.state == hya_store::AdmissionState::Started || record.state.is_terminal()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("durable work must still promote/start after receiver drop");
        let final_record = engine
            .store()
            .admission(operation_id)
            .await
            .unwrap()
            .expect("row");
        assert_ne!(
            final_record.state,
            hya_store::AdmissionState::Cancelled,
            "receiver drop must not durable-cancel the operation"
        );
    }

    #[tokio::test]
    async fn promotion_first_cancel_before_registration() {
        // Consult30 RED 4: Queued -> Accepted commits first; subsequent cancel
        // terminalizes without fabricating a running outcome or registering work.
        let workdir = tempdir();
        let router = Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new())));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
        let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(8);
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "promotion-first base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();

        let filler_operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let filler_intents = (0..100)
            .map(|ordinal| AdmissionIntent {
                runtime_fingerprint_version: 1,
                runtime_fingerprint: [0x11; 32],
                admission_binding_fingerprint_version: 1,
                admission_binding_fingerprint: [0x22; 32],
                spawn_intent: vec![0x33, u8::try_from(ordinal % 200).unwrap()],
            })
            .collect();
        let filler_claim = AdmissionClaim {
            operation_id: filler_operation.operation_id(),
            source_tool_call_id: filler_operation.source_tool_call_id(),
            root_session: parent,
            request_fingerprint: [0x77; 32],
            admission_units: 100,
            actor_claim: None,
        };
        match engine
            .store()
            .claim_admission_batch(&filler_claim, filler_intents)
            .await
            .unwrap()
        {
            AdmissionBatchClaimOutcome::Claimed(launches) => assert_eq!(launches.len(), 100),
            AdmissionBatchClaimOutcome::Existing => panic!("filler must be fresh"),
        }

        let _spawn_supervisor = spawn_team_supervisor_with_environment(
            spawn_rx,
            Arc::clone(&engine),
            base,
            Arc::clone(&router),
            Arc::new(CategoryRegistry::default()),
            ResidentSupervisor::start(Arc::clone(&engine)),
            Arc::new(BundleSidecarEnvironment {
                command: None,
                staging_root: tempdir(),
                terminate_notify: None,
                test_observer: None,
                uniform_probe: None,
            }),
        );

        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_id = operation.operation_id();
        let cancel = CancellationToken::new();
        let spawner = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents);
        let cancel_task = cancel.clone();
        let reply_task = tokio::spawn(async move {
            spawner
                .spawn_background(
                    operation,
                    vec![SpawnMember {
                        description: "promo-first".to_string(),
                        prompt: "PROMO-FIRST".to_string(),
                        subagent_type: "general".to_string(),
                        ..SpawnMember::default()
                    }],
                    cancel_task,
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let record = engine.store().admission(operation_id).await.unwrap();
                if record.is_some_and(|r| r.state == hya_store::AdmissionState::Queued) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background must queue under full capacity");

        // Promotion-first: free a slot; finalize auto-promotes Queued → Accepted.
        // Do not wake the owner, so it cannot cross Started before cancel.
        let release = engine
            .store()
            .finalize_admission_members(
                &[(filler_operation.operation_id(), 0)],
                AdmissionTerminal::Aborted,
                "free slot for promotion-first",
                None,
            )
            .await
            .unwrap();
        assert!(
            release
                .promoted
                .iter()
                .any(|launch| launch.record.operation_id == operation_id
                    && launch.record.state == hya_store::AdmissionState::Accepted),
            "promotion must Accept the background row first: {:?}",
            release.promoted
        );
        let record = engine
            .store()
            .admission(operation_id)
            .await
            .unwrap()
            .expect("background row");
        assert_eq!(record.state, hya_store::AdmissionState::Accepted);
        let sessions_after_promo = engine.store().list_sessions().await.unwrap().len();
        assert_eq!(
            sessions_after_promo, 1,
            "Accepted alone must not register a child session"
        );
        assert!(
            !reply_task.is_finished(),
            "promotion alone must not complete the caller reply"
        );

        // Queued-cancel has lost; post-promotion cancel terminalizes Accepted.
        cancel.cancel();
        let reply = tokio::time::timeout(Duration::from_secs(5), reply_task)
            .await
            .expect("post-promotion cancel reply timed out")
            .expect("join");
        assert!(
            matches!(reply, Err(SpawnError::Cancelled)),
            "post-promotion cancel before registration must not fabricate running: {reply:?}"
        );
        let record = engine
            .store()
            .admission(operation_id)
            .await
            .unwrap()
            .expect("admission row");
        assert_eq!(record.state, hya_store::AdmissionState::Cancelled);
        assert_eq!(
            engine.store().list_sessions().await.unwrap().len(),
            sessions_after_promo,
            "cancel after Accept before registration must not create a session"
        );
    }

    #[tokio::test]
    async fn queued_cancel_wins_before_promotion() {
        // Consult30 RED 3: cancel-first while Queued → durable Cancelled, zero
        // allocation, one SpawnError::Cancelled, and later promotion is empty.
        let workdir = tempdir();
        let router = Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new())));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
        let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(8);
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "cancel-first base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();

        let filler_operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let filler_intents = (0..100)
            .map(|ordinal| AdmissionIntent {
                runtime_fingerprint_version: 1,
                runtime_fingerprint: [0x11; 32],
                admission_binding_fingerprint_version: 1,
                admission_binding_fingerprint: [0x22; 32],
                spawn_intent: vec![0x33, u8::try_from(ordinal % 200).unwrap()],
            })
            .collect();
        let filler_claim = AdmissionClaim {
            operation_id: filler_operation.operation_id(),
            source_tool_call_id: filler_operation.source_tool_call_id(),
            root_session: parent,
            request_fingerprint: [0x66; 32],
            admission_units: 100,
            actor_claim: None,
        };
        match engine
            .store()
            .claim_admission_batch(&filler_claim, filler_intents)
            .await
            .unwrap()
        {
            AdmissionBatchClaimOutcome::Claimed(launches) => assert_eq!(launches.len(), 100),
            AdmissionBatchClaimOutcome::Existing => panic!("filler must be fresh"),
        }

        let _spawn_supervisor = spawn_team_supervisor_with_environment(
            spawn_rx,
            Arc::clone(&engine),
            base,
            Arc::clone(&router),
            Arc::new(CategoryRegistry::default()),
            ResidentSupervisor::start(Arc::clone(&engine)),
            Arc::new(BundleSidecarEnvironment {
                command: None,
                staging_root: tempdir(),
                terminate_notify: None,
                test_observer: None,
                uniform_probe: None,
            }),
        );

        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_id = operation.operation_id();
        let cancel = CancellationToken::new();
        let spawner = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents);
        let cancel_task = cancel.clone();
        let reply_task = tokio::spawn(async move {
            spawner
                .spawn_background(
                    operation,
                    vec![SpawnMember {
                        description: "cancel-queued".to_string(),
                        prompt: "CANCEL-MARKER".to_string(),
                        subagent_type: "general".to_string(),
                        ..SpawnMember::default()
                    }],
                    cancel_task,
                )
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let record = engine.store().admission(operation_id).await.unwrap();
                if record.is_some_and(|r| r.state == hya_store::AdmissionState::Queued) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("member must be Queued before cancel");
        let sessions_before = engine.store().list_sessions().await.unwrap().len();
        cancel.cancel();

        let reply = tokio::time::timeout(Duration::from_secs(5), reply_task)
            .await
            .expect("cancel reply timed out")
            .expect("join");
        assert!(
            matches!(reply, Err(SpawnError::Cancelled)),
            "cancel-first must return SpawnError::Cancelled, got {reply:?}"
        );
        let record = engine
            .store()
            .admission(operation_id)
            .await
            .unwrap()
            .expect("admission row");
        assert_eq!(record.state, hya_store::AdmissionState::Cancelled);
        assert_eq!(
            engine.store().list_sessions().await.unwrap().len(),
            sessions_before,
            "cancel-first must allocate zero child sessions"
        );
        // Free a slot and promote: cancelled row must not re-launch.
        engine
            .store()
            .finalize_admission_members(
                &[(filler_operation.operation_id(), 0)],
                AdmissionTerminal::Aborted,
                "free slot after cancel",
                None,
            )
            .await
            .unwrap();
        let promoted = engine.store().promote_queued_admissions(1).await.unwrap();
        assert!(
            promoted
                .iter()
                .all(|launch| launch.record.operation_id != operation_id),
            "later promotion must not re-launch a Cancelled row"
        );
    }

    #[tokio::test]
    async fn queued_foreground_reply_waits_for_all_terminal() {
        // Consult30 RED 1 (capacity-faithful form): fill 99 active slots, then a
        // 2-member foreground batch yields 1 Accepted + 1 Queued. Reply must wait
        // until both are terminal and preserve ordinal order/identity.
        let workdir = tempdir();
        let provider_gate = Arc::new(ProviderGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let router = Arc::new(ProviderRouter::new().with(Arc::new(CountingDevProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            inner: DevProvider::new(),
            gate: Some(Arc::clone(&provider_gate)),
        })));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
        let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(8);
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "queued batch reply base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();

        let filler_operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let filler_intents = (0..99)
            .map(|ordinal| AdmissionIntent {
                runtime_fingerprint_version: 1,
                runtime_fingerprint: [0x11; 32],
                admission_binding_fingerprint_version: 1,
                admission_binding_fingerprint: [0x22; 32],
                spawn_intent: vec![0x33, u8::try_from(ordinal).unwrap()],
            })
            .collect();
        let filler_claim = AdmissionClaim {
            operation_id: filler_operation.operation_id(),
            source_tool_call_id: filler_operation.source_tool_call_id(),
            root_session: parent,
            request_fingerprint: [0x44; 32],
            admission_units: 99,
            actor_claim: None,
        };
        match engine
            .store()
            .claim_admission_batch(&filler_claim, filler_intents)
            .await
            .unwrap()
        {
            AdmissionBatchClaimOutcome::Claimed(launches) => assert_eq!(launches.len(), 99),
            AdmissionBatchClaimOutcome::Existing => panic!("filler must be fresh"),
        }

        let _spawn_supervisor = spawn_team_supervisor_with_environment(
            spawn_rx,
            Arc::clone(&engine),
            base,
            Arc::clone(&router),
            Arc::new(CategoryRegistry::default()),
            ResidentSupervisor::start(Arc::clone(&engine)),
            Arc::new(BundleSidecarEnvironment {
                command: None,
                staging_root: tempdir(),
                terminate_notify: None,
                test_observer: None,
                uniform_probe: None,
            }),
        );

        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let operation_id = operation.operation_id();
        let members = vec![
            SpawnMember {
                description: "batch-0".to_string(),
                prompt: "BATCH-MARKER-0".to_string(),
                subagent_type: "general".to_string(),
                ..SpawnMember::default()
            },
            SpawnMember {
                description: "batch-1".to_string(),
                prompt: "BATCH-MARKER-1".to_string(),
                subagent_type: "general".to_string(),
                ..SpawnMember::default()
            },
        ];
        let spawner = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents);
        let reply_task = tokio::spawn(async move {
            spawner
                .spawn(operation, members, CancellationToken::new())
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), provider_gate.entered.notified())
            .await
            .expect("accepted member must enter provider gate");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let records = engine.store().admissions(operation_id).await.unwrap();
                if records.len() == 2
                    && records[0].state == hya_store::AdmissionState::Started
                    && records[1].state == hya_store::AdmissionState::Queued
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("batch must split into Started + Queued under full active capacity");
        assert!(
            !reply_task.is_finished(),
            "foreground reply must wait while any member is nonterminal"
        );

        // Release member 0; promotion should start member 1 and re-enter the gate.
        let second_enter = provider_gate.entered.notified();
        provider_gate.release.notify_waiters();
        tokio::time::timeout(Duration::from_secs(10), second_enter)
            .await
            .expect("promoted queued member must enter provider after member 0 completes");
        provider_gate.release.notify_waiters();
        let reply = tokio::time::timeout(Duration::from_secs(30), reply_task)
            .await
            .expect("whole-batch reply timed out")
            .expect("join")
            .expect("mixed Accepted/Queued foreground batch must reply Ok after all terminal");
        assert_eq!(reply.len(), 2);
        assert!(reply.iter().all(|o| o.status == "done"), "{reply:?}");
        assert!(
            reply[0].summary.contains("BATCH-MARKER-0")
                && reply[1].summary.contains("BATCH-MARKER-1"),
            "ordinal-ordered identity must survive queued promotion: {reply:?}"
        );
        let terminal = engine.store().admissions(operation_id).await.unwrap();
        assert!(terminal.iter().all(|r| r.state.is_terminal()));
    }

    #[tokio::test]
    async fn built_session_engine_shutdown_drains_supervisor() {
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let mut built = build_session_engine(
            SessionStore::connect_memory().await.unwrap(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();
        let engine = built.engine();
        assert!(
            engine
                .tool_schemas()
                .iter()
                .any(|s| s.name.as_str() == "bash")
        );
        built
            .shutdown()
            .await
            .expect("explicit shutdown must drain the spawn supervisor");
        // Second shutdown is idempotent (no supervisor join left).
        built
            .shutdown()
            .await
            .expect("idempotent shutdown after drain");
    }

    /// Lifecycle stop must cancel and drain the Workflow request that currently
    /// owns the single worker slot, even when its provider never returns.
    #[tokio::test]
    async fn workflow_supervisor_stop_drains_an_in_flight_run() {
        let workdir = tempdir();
        let workflow_dir = workdir.join(".hya/workflows");
        std::fs::create_dir_all(&workflow_dir).unwrap();
        std::fs::write(
            workflow_dir.join("shutdown-flow.hya.md"),
            r#"---
kind: Workflow
name: shutdown-flow
description: Hold one governed member until lifecycle shutdown.
nodes:
  hold:
    agent: explore
    directive: WAIT FOR SHUTDOWN
---
flowchart TD
  hold
"#,
        )
        .unwrap();

        let provider_gate = Arc::new(ProviderGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let router = Arc::new(ProviderRouter::new().with(Arc::new(CountingDevProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            inner: DevProvider::new(),
            gate: Some(Arc::clone(&provider_gate)),
        })));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _permission_rx) =
            PermissionPlane::new(PermissionRules::new(vec![Rule::new(
                Action::Task,
                "*",
                Mode::Allow,
            )]));
        let (workflow_sender, workflow_rx) = BoundWorkflowSender::with_capacity(1);
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                router,
                runtime,
                permission.clone(),
                EventBus::default(),
            )
            .with_workflow_sender(workflow_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "build".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let lead = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine
            .agent_roster_for_binding(&binding, base.name.as_str())
            .unwrap();
        let workflows = workflow_sender
            .for_binding(&binding)
            .for_session_with_agents(lead, agents);
        let stop = CancellationToken::new();
        let resident_supervisor = ResidentSupervisor::start(Arc::clone(&engine));
        let supervisor = spawn_workflow_supervisor(
            workflow_rx,
            Arc::clone(&engine),
            base,
            resident_supervisor,
            stop.clone(),
        );
        let (interaction, _interaction_rx) = InteractionPlane::new();
        let (spawner, _spawner_rx) = SpawnerPlane::new();
        let ctx = ToolCtx {
            workflows,
            permission: permission.for_session(lead),
            interaction: interaction.for_session(lead),
            spawner,
            operation: ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
            mailbox: MailboxPlane::disconnected(),
            session: Some(lead),
            parent_session: None,
            todo: TodoPlane::default(),
            skills: SkillPlane::default(),
            websearch: WebSearchPlane::default(),
            lsp: LspPlane::default(),
            formatter: FormatterPlane::default(),
            agents: Default::default(),
            workdir,
            cancel: CancellationToken::new(),
        };
        let run = tokio::spawn(async move {
            hya_tool::WorkflowTool
                .execute(&ctx, json!({"action": "run", "name": "shutdown-flow"}))
                .await
        });

        tokio::time::timeout(Duration::from_secs(2), provider_gate.entered.notified())
            .await
            .expect("Workflow member must reach the gated provider");
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(2), supervisor)
            .await
            .expect("Workflow supervisor must stop after cancelling its in-flight run")
            .expect("Workflow supervisor must not panic");
        let output = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .expect("cancelled Workflow tool request must receive a terminal reply")
            .expect("Workflow tool task must not panic")
            .expect("Workflow cancellation is a terminal run outcome");
        assert_eq!(output["metadata"]["status"], "cancelled");
    }

    /// Explicit shutdown must still abort in-flight handlers.
    ///
    /// Only a closed intake drains them (see the supervisor's request-loop exit
    /// branch), so the abort now sits behind the stop token. This pins the other
    /// half of that condition: the member is parked in the gated provider and
    /// will never finish on its own, so a supervisor that drained instead of
    /// aborting would leave `shutdown()` waiting until the timeout fires.
    #[tokio::test]
    async fn shutdown_aborts_in_flight_foreground_handler() {
        let workdir = tempdir();
        let provider_gate = Arc::new(ProviderGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let router = Arc::new(ProviderRouter::new().with(Arc::new(CountingDevProvider {
            calls: Arc::new(AtomicUsize::new(0)),
            inner: DevProvider::new(),
            gate: Some(Arc::clone(&provider_gate)),
        })));
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            builtin_agent_catalog().unwrap(),
        ));
        let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
        let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(4);
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory().await.unwrap(),
                Arc::clone(&router),
                runtime,
                permission,
                EventBus::default(),
            )
            .with_spawn_sender(spawn_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("dev"),
            system_prompt: "shutdown abort base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let parent = engine
            .create(CreateSession {
                parent: None,
                agent: base.name.clone(),
                model: base.model.clone(),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .unwrap();
        let binding = engine.bind_runtime(&workdir).unwrap();
        let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();
        let mut lifecycle = spawn_team_supervisor_with_environment(
            spawn_rx,
            Arc::clone(&engine),
            base,
            Arc::clone(&router),
            Arc::new(CategoryRegistry::default()),
            ResidentSupervisor::start(Arc::clone(&engine)),
            Arc::new(BundleSidecarEnvironment {
                command: None,
                staging_root: tempdir(),
                terminate_notify: None,
                test_observer: None,
                uniform_probe: None,
            }),
        );

        let scoped = spawn_sender
            .for_binding(&binding)
            .for_session_with_agents(parent, agents);
        let _in_flight = tokio::spawn(async move {
            scoped
                .spawn(
                    ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
                    vec![SpawnMember {
                        description: "in-flight member".to_string(),
                        prompt: "SHUTDOWN-ABORT-MARKER".to_string(),
                        subagent_type: "general".to_string(),
                        ..SpawnMember::default()
                    }],
                    CancellationToken::new(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), provider_gate.entered.notified())
            .await
            .expect("admitted member must reach the gated provider before shutdown");

        tokio::time::timeout(Duration::from_secs(10), lifecycle.shutdown())
            .await
            .expect("shutdown must abort the gated in-flight handler")
            .expect("supervisor must join cleanly after shutdown");
    }

    #[tokio::test]
    async fn built_session_engine_drop_is_nonblocking() {
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let built = build_session_engine(
            SessionStore::connect_memory().await.unwrap(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();
        // Drop must not await drain; it only signals stop/abort.
        drop(built);
    }

    #[tokio::test]
    async fn pre_started_admission_failure_cancels_task_before_recovery() {
        struct DropSentinel(Arc<AtomicUsize>);

        impl Drop for DropSentinel {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let database = tempdir().join("pre-start-failure.db");
        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();
        let operation = ToolOperation::from_tool_call(hya_proto::ToolCallId::new());
        let root_session = SessionId::new();
        let intent = SpawnIntentV1::new(SpawnIntentInputV1 {
            member: SpawnMember {
                description: "pre-start-failure-description".to_string(),
                prompt: "pre-start-failure-prompt".to_string(),
                subagent_type: "general".to_string(),
                task_id: None,
                model: None,
                category: None,
                inline_agent: None,
                resident: false,
            },
            parent: root_session,
            stable_target: AgentName::new("general"),
            background: true,
            operation,
            member_ordinal: 0,
            batch_cardinality: 1,
            prior_start: PriorStartV1::NeverStarted,
            runtime_fingerprint: [0x11; 32],
            admission_binding_fingerprint: [0x22; 32],
            diagnostic_generation: 7,
        })
        .expect("one-member intent must be canonical")
        .into_admission_intent()
        .expect("one-member intent must encode");
        let claim = hya_store::AdmissionClaim {
            operation_id: operation.operation_id(),
            source_tool_call_id: operation.source_tool_call_id(),
            root_session,
            request_fingerprint: [0x33; 32],
            admission_units: 1,
            actor_claim: None,
        };
        let launches = match store
            .claim_admission_batch(&claim, vec![intent])
            .await
            .expect("one-member admission claim must succeed")
        {
            hya_store::AdmissionBatchClaimOutcome::Claimed(launches) => launches,
            hya_store::AdmissionBatchClaimOutcome::Existing => {
                panic!("fresh temporary store must claim one admission launch")
            }
        };
        assert_eq!(launches.len(), 1);
        let launch = launches.into_iter().next().unwrap();
        assert_eq!(launch.record.state, hya_store::AdmissionState::Accepted);
        assert!(launch.record.actor.is_none());

        let drop_count = Arc::new(AtomicUsize::new(0));
        let work_count = Arc::new(AtomicUsize::new(0));
        let sentinel = DropSentinel(Arc::clone(&drop_count));
        let work_count_for_task = Arc::clone(&work_count);
        let work_future = async move {
            let _sentinel = sentinel;
            work_count_for_task.fetch_add(1, Ordering::SeqCst);
        };
        let installed = install_admission_task(&launch, work_future);

        tokio::task::yield_now().await;
        assert_eq!(work_count.load(Ordering::SeqCst), 0);
        assert_eq!(drop_count.load(Ordering::SeqCst), 0);
        let before_start = store
            .admission(operation.operation_id())
            .await
            .expect("read accepted admission row")
            .expect("accepted admission row must exist");
        assert_eq!(before_start.state, hya_store::AdmissionState::Accepted);
        assert!(before_start.actor.is_none());

        let stale_actor_claim = hya_store::ActorClaim {
            actor_id: SessionId::new(),
            epoch: hya_proto::ActorEpoch::from_storage(1),
            owner_run_id: OwnerRunId::new(),
        };
        match installed.start(&store, Some(&stale_actor_claim)).await {
            Err(SpawnError::Unavailable) => {}
            Ok(_) => panic!("stale actor claim must not start an admission task"),
            Err(error) => panic!("unexpected admission start error: {error:?}"),
        }
        assert_eq!(work_count.load(Ordering::SeqCst), 0);
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
        let after_start_failure = store
            .admission(operation.operation_id())
            .await
            .expect("read accepted admission row after failed start")
            .expect("accepted admission row must remain present");
        assert_eq!(
            after_start_failure.state,
            hya_store::AdmissionState::Accepted
        );
        assert!(after_start_failure.actor.is_none());

        let recovered = store
            .recover_nonterminal_admissions("pre-start failure recovery")
            .await
            .expect("accepted admission recovery must succeed");
        assert_eq!(recovered.len(), 1);
        let recovered_record = recovered.into_iter().next().unwrap();
        assert_eq!(recovered_record.operation_id, operation.operation_id());
        assert_eq!(recovered_record.state, hya_store::AdmissionState::Queued);
        assert!(recovered_record.actor.is_none());
        assert_eq!(
            store.admission_counts().await.unwrap(),
            hya_store::AdmissionCounts {
                active: 0,
                non_active: 1,
                total: 1,
            }
        );

        let repeated = store
            .recover_nonterminal_admissions("pre-start failure recovery")
            .await
            .expect("repeated admission recovery must succeed");
        assert!(repeated.is_empty());
        let persisted = store
            .admission(operation.operation_id())
            .await
            .expect("read recovered admission row")
            .expect("recovered admission row must remain present");
        assert_eq!(persisted, recovered_record);
        assert_eq!(persisted.state, hya_store::AdmissionState::Queued);
    }

    struct RuntimeMarker(&'static str);

    fn mcp_fixture() -> Vec<String> {
        vec![
            "python3".to_string(),
            "-c".to_string(),
            r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    if req["method"] == "initialize":
        result = {"capabilities": {}}
    elif req["method"] == "tools/list":
        result = {"tools": [{"name":"ping","description":"Ping","inputSchema":{"type":"object"}}]}
    else:
        result = {"content":{"ok":True},"isError":False}
    print(json.dumps({"jsonrpc":"2.0","id":req["id"],"result":result}), flush=True)
"#
            .to_string(),
        ]
    }

    fn plugin_fixture(id: &str) -> Vec<String> {
        vec![
            "python3".to_string(),
            "-c".to_string(),
            format!(
                r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if req.get("method") == "initialize":
        result = {{
            "protocol_version":1,
            "plugin":{{"id":"{id}","version":"1","kind":"rust"}},
            "hooks":[],
            "tools":[{{"name":"plugin_ping","description":"Ping","inputSchema":{{"type":"object"}}}}]
        }}
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":result}}), flush=True)
    elif req.get("method") == "shutdown":
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":{{}}}}), flush=True)
        sys.exit(0)
    elif "id" in req:
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":{{"ok":True,"output":{{}}}}}}), flush=True)
"#
            ),
        ]
    }

    #[async_trait]
    impl Tool for RuntimeMarker {
        fn name(&self) -> &str {
            self.0
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new(self.0),
                description: format!("{} runtime marker", self.0),
                input_schema: json!({ "type": "object" }),
                output_schema: None,
            }
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
            Ok(json!({ "ok": true }))
        }
    }

    struct EnvGuard {
        _lock: std::sync::RwLockWriteGuard<'static, ()>,
        home: Option<std::ffi::OsString>,
        xdg_config_home: Option<std::ffi::OsString>,
        xdg_data_home: Option<std::ffi::OsString>,
        current_dir: PathBuf,
    }

    impl EnvGuard {
        fn set(home: &Path, cwd: &Path) -> Self {
            let lock = ENV_LOCK
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let guard = Self {
                _lock: lock,
                home: std::env::var_os("HOME"),
                xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
                xdg_data_home: std::env::var_os("XDG_DATA_HOME"),
                current_dir: std::env::current_dir().unwrap(),
            };
            std::fs::create_dir_all(home).unwrap();
            std::fs::create_dir_all(cwd).unwrap();
            unsafe {
                std::env::set_var("HOME", home);
                std::env::set_var("XDG_CONFIG_HOME", home);
                std::env::set_var("XDG_DATA_HOME", home);
            }
            std::env::set_current_dir(cwd).unwrap();
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.current_dir);
            unsafe {
                if let Some(home) = &self.home {
                    std::env::set_var("HOME", home);
                } else {
                    std::env::remove_var("HOME");
                }
                if let Some(xdg_config_home) = &self.xdg_config_home {
                    std::env::set_var("XDG_CONFIG_HOME", xdg_config_home);
                } else {
                    std::env::remove_var("XDG_CONFIG_HOME");
                }
                if let Some(xdg_data_home) = &self.xdg_data_home {
                    std::env::set_var("XDG_DATA_HOME", xdg_data_home);
                } else {
                    std::env::remove_var("XDG_DATA_HOME");
                }
            }
        }
    }

    fn tempdir() -> PathBuf {
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let serial = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hya-app-runtime-test-{nanos}-{serial}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(dir: &Path, name: &str, description: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn offline_notice_names_path_offline_mode_and_the_fix() {
        let notice = OfflineNotice {
            config_path: PathBuf::from("/home/u/.config/hya/config.yaml"),
        };
        let text = notice.render();
        // (a) where the missing config is expected,
        assert!(text.contains("/home/u/.config/hya/config.yaml"));
        // (b) that we are in offline/echo mode,
        assert!(text.contains("OFFLINE"));
        assert!(text.contains("echoes"));
        // (c) how to fix it.
        assert!(text.contains("hya login"));
        assert!(text.contains("docs/configuration.md"));
    }

    #[test]
    fn resolve_runtime_without_config_carries_but_does_not_print_the_notice() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        let _ = std::fs::remove_file(&config_path);

        let runtime = resolve_runtime(None);

        // Offline fallback selected: the built-in echo provider + "offline" model.
        assert_eq!(runtime.model, "offline");
        // The guidance is returned as DATA — resolve_runtime itself prints
        // nothing, so headless/RPC/serve callers (which never call `emit`) keep
        // a clean machine-readable stdout. Only interactive startup emits it.
        let notice = runtime
            .offline_notice
            .expect("missing-config path must carry an offline notice");
        assert!(notice.config_path.ends_with("hya/config.yaml"));
        assert!(notice.render().contains("OFFLINE"));
    }

    #[test]
    fn permission_only_config_is_kept_and_config_errors_fall_back_to_strict() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "permission:\n  model: allow\n").unwrap();

        let runtime = resolve_runtime(None);
        assert_eq!(runtime.model, "offline");
        assert_eq!(runtime.permission.model(), PermissionModel::Allow);
        assert!(runtime.offline_notice.is_none());
        assert_eq!(
            runtime.with_yolo(true).permission.model(),
            PermissionModel::Danger
        );

        std::fs::write(
            &config_path,
            "permission:\n  rules:\n    - target: tool\n      selector: '('\n      permission: Allow\n",
        )
        .unwrap();
        let fallback = resolve_runtime(None);
        assert_eq!(fallback.permission.model(), PermissionModel::Strict);
    }

    #[test]
    fn websearch_only_config_reaches_runtime() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "tools:\n  websearch:\n    provider: parallel\n    endpoint: https://search.example.test/mcp\n    key: secret\n    enabled: false\n",
        )
        .unwrap();

        let runtime = resolve_runtime(None);

        assert_eq!(
            runtime.websearch.provider,
            hya_tool::WebSearchProvider::Parallel
        );
        assert_eq!(
            runtime.websearch.endpoint.as_deref(),
            Some("https://search.example.test/mcp")
        );
        assert_eq!(runtime.websearch.key.as_deref(), Some("secret"));
        assert!(!runtime.websearch.enabled);
        assert!(runtime.offline_notice.is_none());
    }

    #[tokio::test]
    async fn disabled_websearch_is_not_exposed_by_engine() {
        let store = SessionStore::connect_memory().await.unwrap();
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let mut built = build_session_engine(
            store,
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (
                WebSearchConfig {
                    enabled: false,
                    ..WebSearchConfig::default()
                },
                InvocationPolicy::default(),
            ),
        )
        .await
        .unwrap();
        let engine = built.engine();
        let _asks = built.take_asks();
        let _questions = built.take_questions();
        let _mcp = built.mcp_control();
        let _plugins = built.plugin_host();
        let _built = built;

        assert!(
            engine
                .tool_schemas()
                .iter()
                .all(|schema| schema.name.as_str() != "websearch")
        );
    }

    #[tokio::test]
    async fn built_engine_lazily_refreshes_installed_catalog_at_root_binding() {
        let home = tempdir();
        let workdir = tempdir();
        let _env = EnvGuard::set(&home, &workdir);
        let registry_path = home.join("hya/bundles/registry.sqlite3");
        assert!(!registry_path.exists());

        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let mut built = build_session_engine(
            SessionStore::connect_memory().await.unwrap(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();
        let engine = built.engine();
        let _asks = built.take_asks();
        let _questions = built.take_questions();
        let _mcp = built.mcp_control();
        let _plugins = built.plugin_host();
        let _built = built;
        assert!(!registry_path.exists());

        let old_binding = engine.bind_runtime(&workdir).unwrap();
        assert!(
            old_binding
                .resolve_agent("runtime-installed-agent")
                .is_none()
        );

        std::fs::create_dir_all(registry_path.parent().unwrap()).unwrap();
        let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
            .await
            .unwrap();
        let installed = prepare_package(BundleSource::new(
            "runtime-installed",
            vec![SourceFile::new(
                "bundle.hya.md",
                br#"---
kind: AgentBundle
identity:
  id: hya/runtime-installed-test
  version: 1.0.0
  publisher: hya
agent:
  id: runtime-installed-agent
  role: main
  spawn_lifecycle: transient
---
You are the runtime-installed agent.
"#,
            )],
        ))
        .unwrap();
        let outcome = registry
            .install(
                &[],
                BundleInstallCandidate {
                    source_digest: [0x52; 32],
                    prepared_digest: installed.digest().to_string(),
                    prepared_bytes: installed.bytes().to_vec(),
                    installed_at: 1_725_000_020,
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome, BundleInstallOutcome::Installed { generation: 1 });

        let fresh_binding = engine.bind_root_runtime(&workdir).await.unwrap();
        assert!(
            fresh_binding
                .resolve_agent("runtime-installed-agent")
                .is_some()
        );
        assert!(
            old_binding
                .resolve_agent("runtime-installed-agent")
                .is_none()
        );
    }

    #[tokio::test]
    async fn engine_snapshot_rejects_builder_bypass_and_publishes_deferred_set_atomically() {
        let (router, _model) = offline_router(None);
        let builder = Arc::new(ToolRegistry::builtins());
        let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
        let engine = Arc::new(SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(router),
            Arc::new(RuntimeRegistry::from_snapshot(
                builder.snapshot(),
                builtin_agent_catalog().unwrap(),
            )),
            permission,
            EventBus::default(),
        ));

        builder
            .register(Arc::new(RuntimeMarker("builder_bypass")))
            .unwrap();
        assert!(
            engine
                .tool_schemas()
                .iter()
                .all(|schema| schema.name.as_str() != "builder_bypass"),
            "mutating the retained candidate builder must not change the effective snapshot"
        );

        let saw_complete_old_view = Arc::new(AtomicBool::new(false));
        let observed = saw_complete_old_view.clone();
        let inspect_engine = engine.clone();
        let mut next = 0;
        let deferred_tools = std::iter::from_fn(move || {
            let item = match next {
                0 => Some(Arc::new(RuntimeMarker("mcp__deferred__first")) as Arc<dyn Tool>),
                1 => {
                    let visible = inspect_engine
                        .tool_schemas()
                        .into_iter()
                        .map(|schema| schema.name.to_string())
                        .collect::<Vec<_>>();
                    observed.store(
                        !visible.iter().any(|name| name == "mcp__deferred__first")
                            && !visible.iter().any(|name| name == "mcp__deferred__second"),
                        Ordering::SeqCst,
                    );
                    Some(Arc::new(RuntimeMarker("mcp__deferred__second")) as Arc<dyn Tool>)
                }
                _ => None,
            };
            next += 1;
            item
        });

        engine
            .refresh_runtime(|candidate| {
                for tool in deferred_tools {
                    candidate.register_tool_with_permission(tool, ToolPermission::Mcp)?;
                }
                Ok(())
            })
            .unwrap();

        assert!(
            saw_complete_old_view.load(Ordering::SeqCst),
            "the first candidate member became visible before atomic publication"
        );
        let visible = engine
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.to_string())
            .collect::<Vec<_>>();
        assert!(visible.iter().any(|name| name == "mcp__deferred__first"));
        assert!(visible.iter().any(|name| name == "mcp__deferred__second"));
        assert!(!visible.iter().any(|name| name == "builder_bypass"));
    }

    #[tokio::test]
    async fn engine_build_aborts_nonterminal_admissions_before_spawn_readiness() {
        let database = tempdir().join("admission-recovery.db");
        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();
        let source_tool_call_id = hya_proto::ToolCallId::new();
        let operation_id = hya_proto::OperationId::from_tool_call(source_tool_call_id);
        let root_session = SessionId::new();
        store
            .claim_admission(&hya_store::AdmissionClaim {
                operation_id,
                source_tool_call_id,
                root_session,
                request_fingerprint: [17; 32],
                admission_units: 1,
                actor_claim: None,
            })
            .await
            .unwrap();
        store.start_admission(operation_id, None).await.unwrap();
        drop(store);
        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);

        let _built = build_session_engine(
            store.clone(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();

        let recovered = store.admission(operation_id).await.unwrap().unwrap();
        assert_eq!(recovered.state, hya_store::AdmissionState::Aborted);
        assert!(recovered.logical_released);
        assert!(store.replay(root_session).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn engine_build_fences_running_resident_and_resumes_queued_mail_before_readiness() {
        let database = tempdir().join("resident-recovery.db");
        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let queued_root = SessionId::new();
        let queued_actor = SessionId::new();
        let running_root = SessionId::new();
        let running_actor = SessionId::new();

        for (root, actor) in [(queued_root, queued_actor), (running_root, running_actor)] {
            store
                .append_event(
                    root,
                    &Event::SessionCreated {
                        session: root,
                        parent: None,
                        agent: agent.name.clone(),
                        model: agent.model.clone(),
                        workdir: agent.workdir.to_string_lossy().into_owned(),
                    },
                )
                .await
                .unwrap();
            store
                .append_event(
                    actor,
                    &Event::SessionCreated {
                        session: actor,
                        parent: Some(root),
                        agent: agent.name.clone(),
                        model: agent.model.clone(),
                        workdir: agent.workdir.to_string_lossy().into_owned(),
                    },
                )
                .await
                .unwrap();
        }

        let queued_claim = store
            .try_claim_new(queued_actor, OwnerRunId::new())
            .await
            .unwrap();
        store
            .commit_resident_mutation(
                &queued_claim,
                queued_root,
                &[Event::AgentRegistered {
                    session: queued_root,
                    agent_session: queued_actor,
                    handle: "queued-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: agent.name.clone(),
                    mode: SubagentMode::Resident,
                }],
            )
            .await
            .unwrap();
        store
            .append_event(
                queued_root,
                &Event::MailSent {
                    session: queued_root,
                    from: "main".to_string(),
                    to: MailEndpoint::Handle("main/queued-1".to_string()),
                    kind: MailKind::Message,
                    body: "resume me".to_string(),
                },
            )
            .await
            .unwrap();

        let running_claim = store
            .try_claim_new(running_actor, OwnerRunId::new())
            .await
            .unwrap();
        store
            .commit_resident_mutation(
                &running_claim,
                running_root,
                &[
                    Event::AgentRegistered {
                        session: running_root,
                        agent_session: running_actor,
                        handle: "running-1".to_string(),
                        parent: Some("main".to_string()),
                        agent_type: agent.name.clone(),
                        mode: SubagentMode::Resident,
                    },
                    Event::ResidentWorkStarted {
                        session: running_root,
                        actor_session: running_actor,
                        handle: "main/running-1".to_string(),
                        epoch: running_claim.epoch,
                        inbox_through: 0,
                    },
                ],
            )
            .await
            .unwrap();
        drop(store);

        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();
        let mut built = build_session_engine(
            store.clone(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();
        let _engine = built.engine();
        let _asks = built.take_asks();
        let _questions = built.take_questions();
        let _mcp = built.mcp_control();
        let _plugins = built.plugin_host();
        let _built = built;

        let running = store.read_projection(running_root).await.unwrap();
        let running_entry = running.team.roster.get("main/running-1").unwrap();
        assert_eq!(running_entry.status, RosterStatus::Failed);
        assert!(running_entry.resident_work.is_none());

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let queued = store.read_projection(queued_root).await.unwrap();
                let entry = queued.team.roster.get("main/queued-1").unwrap();
                if entry.resident_cursor == 1 || entry.resident_work.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn startup_recovery_rebuilds_executable_resident_sidecar_before_queued_mail() {
        let home = tempdir();
        let workdir = tempdir();
        let _env = EnvGuard::set(&home, &workdir);
        let registry_path = bundle_registry_path();
        std::fs::create_dir_all(registry_path.parent().unwrap()).unwrap();
        let stable_id = "runtime-installed-resident-agent";
        let marker_path = home.join("resident-startup-sidecar.marker");
        let marker_literal =
            serde_json::to_string(&marker_path.to_string_lossy().to_string()).unwrap();
        let installed = prepare_package(BundleSource::new(
            "runtime-installed-resident",
            vec![
                SourceFile::new(
                    "bundle.hya.md",
                    br#"---
kind: AgentBundle
identity:
  id: hya/runtime-installed-resident
  version: 1.0.0
  publisher: hya
resources:
  tools:
    - id: echo
      path: extensions/runtime.js
extensions:
  js:
    - id: runtime
      path: extensions/runtime.js
agent:
  id: runtime-installed-resident-agent
  role: subagent
  spawn_lifecycle: resident
  resource_view:
    allow:
      - bundle:hya/runtime-installed-resident/tool/echo
---
You are the installed resident agent.
"#,
                ),
                SourceFile::new(
                    "extensions/runtime.js",
                    format!(
                        r#"export default {{
  id: "runtime",
  server: async () => {{
    await Bun.write({marker_literal}, "ready");
    return {{
      tool: {{
        echo: {{
          description: "startup recovery echo",
          execute: async () => "startup-recovery-echo",
        }},
      }},
    }};
  }},
}}
"#
                    ),
                ),
            ],
        ))
        .unwrap();
        let registry = BundleRegistry::connect(registry_path.to_str().unwrap())
            .await
            .unwrap();
        let outcome = registry
            .install(
                &[],
                BundleInstallCandidate {
                    source_digest: [0x53; 32],
                    prepared_digest: installed.digest().to_string(),
                    prepared_bytes: installed.bytes().to_vec(),
                    installed_at: 1_725_000_021,
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome, BundleInstallOutcome::Installed { generation: 1 });

        let database = home.join("resident-startup-recovery.db");
        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();
        let (router, model) = offline_router(None);
        let base = agent_with_model(&model, None);
        let root = SessionId::new();
        let actor = SessionId::new();
        store
            .append_event(
                root,
                &Event::SessionCreated {
                    session: root,
                    parent: None,
                    agent: base.name.clone(),
                    model: base.model.clone(),
                    workdir: workdir.to_string_lossy().into_owned(),
                },
            )
            .await
            .unwrap();
        store
            .append_event(
                actor,
                &Event::SessionCreated {
                    session: actor,
                    parent: Some(root),
                    agent: AgentName::new(stable_id),
                    model: base.model.clone(),
                    workdir: workdir.to_string_lossy().into_owned(),
                },
            )
            .await
            .unwrap();
        let claim = store.try_claim_new(actor, OwnerRunId::new()).await.unwrap();
        store
            .commit_resident_mutation(
                &claim,
                root,
                &[Event::AgentRegistered {
                    session: root,
                    agent_session: actor,
                    handle: "installed-resident-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new(stable_id),
                    mode: SubagentMode::Resident,
                }],
            )
            .await
            .unwrap();
        store
            .append_event(
                root,
                &Event::MailSent {
                    session: root,
                    from: "main".to_string(),
                    to: MailEndpoint::Handle("main/installed-resident-1".to_string()),
                    kind: MailKind::Message,
                    body: "startup recovery executable mail".to_string(),
                },
            )
            .await
            .unwrap();
        let observed_store = store.clone();

        let mut built = build_session_engine(
            store,
            router,
            &base,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .expect("startup recovery must resolve the installed resident definition");
        let engine = built.engine();
        let _asks = built.take_asks();
        let _questions = built.take_questions();
        let _mcp = built.mcp_control();
        let _plugins = built.plugin_host();
        let _built = built;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let projection = observed_store.read_projection(root).await.unwrap();
                let Some(entry) = projection.team.roster.get("main/installed-resident-1") else {
                    tokio::task::yield_now().await;
                    continue;
                };
                if entry.resident_cursor >= 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("startup recovery must consume queued resident mail");
        assert_eq!(
            std::fs::read_to_string(&marker_path).unwrap_or_default(),
            "ready",
            "startup recovery must reconstruct and ACK executable sidecar before consuming queued mail"
        );
        let binding = engine
            .bind_runtime(&workdir)
            .expect("current binding must be available after startup recovery");
        assert_eq!(
            binding
                .resolve_agent(stable_id)
                .expect("installed resident must be in the current catalog")
                .stable_id,
            stable_id
        );
    }

    #[tokio::test]
    async fn deferred_mcp_returns_before_slow_child_handshake() {
        let store = SessionStore::connect_memory().await.unwrap();
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let mut mcp = BTreeMap::new();
        mcp.insert(
            "slow".to_string(),
            hya_mcp::McpServerConfig {
                // Sleep longer than the assert budget so classic await-before-listen would fail.
                command: vec!["sleep".into(), "30".into()],
                ..hya_mcp::McpServerConfig::default()
            },
        );
        let started = std::time::Instant::now();
        let result = build_session_engine_with_mcp_defer(
            store,
            router,
            &agent,
            mcp,
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
            EngineBuildOptions { defer_mcp: true },
        )
        .await;
        let mut built = result.unwrap();
        let _engine = built.engine();
        let _asks = built.take_asks();
        let _questions = built.take_questions();
        let mcp_control = built.mcp_control();
        let _plugins = built.plugin_host();
        let _built = built;
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "build_session_engine blocked on MCP for {elapsed:?}"
        );
        assert_eq!(
            mcp_control.status().await.get("slow"),
            Some(&hya_mcp::McpStatus::Connecting)
        );
    }

    #[tokio::test]
    async fn startup_mixed_mcp_plugin_publishes_one_complete_generation() {
        let mut mcp = BTreeMap::new();
        mcp.insert(
            "mixed".to_string(),
            McpServerConfig {
                command: mcp_fixture(),
                timeout_ms: Some(1_000),
                ..McpServerConfig::default()
            },
        );
        let plugins = vec![PluginSpec {
            id: "mixed-plugin".to_string(),
            kind: hya_plugin::messages::PluginKindWire::Rust,
            command: plugin_fixture("mixed-plugin"),
            timeout_ms: Some(1_000),
            env: BTreeMap::new(),
            posture_overrides: BTreeMap::new(),
        }];
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let result = build_session_engine_with_mcp_defer(
            SessionStore::connect_memory().await.unwrap(),
            router,
            &agent,
            mcp,
            plugins,
            (WebSearchConfig::default(), InvocationPolicy::default()),
            EngineBuildOptions { defer_mcp: false },
        )
        .await;
        let mut built = result.unwrap();
        let engine = built.engine();
        let _ = built.take_asks();
        let _ = built.take_questions();
        let _built = built;
        let manifest = engine.runtime_registry().effective_manifest();
        assert_eq!(
            manifest.generation.get(),
            hya_proto::ConfigGeneration::INITIAL.get() + 1
        );
        assert!(manifest.sources.contains_key(&SourceId::mcp("mixed")));
        assert!(
            manifest
                .sources
                .contains_key(&SourceId::plugin("mixed-plugin"))
        );
        let names = engine
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"mcp__mixed__ping".to_string()));
        assert!(names.contains(&"plugin_ping".to_string()));
    }

    #[tokio::test]
    async fn compat_mcp_control_publishes_and_removes_through_one_runtime_registry() {
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let mut built = build_session_engine(
            SessionStore::connect_memory().await.unwrap(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();
        let engine = built.engine();
        let _ = built.take_asks();
        let _ = built.take_questions();
        let control = built.mcp_control();
        let _built = built;
        let workdir = tempdir();
        let before = engine.runtime_registry().bind_turn(&workdir).unwrap();
        control
            .upsert(
                "dynamic".to_string(),
                McpServerConfig {
                    command: mcp_fixture(),
                    timeout_ms: Some(1_000),
                    ..McpServerConfig::default()
                },
            )
            .await
            .unwrap();
        let connected = engine.runtime_registry().bind_turn(&workdir).unwrap();
        assert!(before.resolve_tool("mcp__dynamic__ping").is_none());
        assert!(connected.resolve_tool("mcp__dynamic__ping").is_some());
        assert_eq!(
            control.status().await.get("dynamic"),
            Some(&hya_mcp::McpStatus::Connected)
        );

        assert!(
            control
                .set_enabled("dynamic".to_string(), false)
                .await
                .unwrap()
        );
        let removed = engine.runtime_registry().bind_turn(&workdir).unwrap();
        assert!(removed.resolve_tool("mcp__dynamic__ping").is_none());
        assert!(connected.resolve_tool("mcp__dynamic__ping").is_some());
        assert_eq!(
            control.status().await.get("dynamic"),
            Some(&hya_mcp::McpStatus::Disabled)
        );
    }

    #[test]
    fn selected_model_reasoning_default_reaches_first_agent() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "default_model: gateway/gpt-5.6-sol\nproviders:\n  gateway:\n    kind: openai-response\n    base_url: https://example.test/v1\n    api_key: test\n    models:\n      - id: gpt-5.6-sol\n        reasoning:\n          default: medium\n          variants: [low, medium]\n",
        )
        .unwrap();

        let runtime = resolve_runtime(None);

        assert_eq!(
            runtime.reasoning,
            Some(hya_provider::ReasoningEffort::Medium)
        );
        assert_eq!(
            runtime.router.catalog()[0].reasoning_variants,
            ["low", "medium"]
        );
        let agent = agent_with_model(&runtime.model, runtime.reasoning);
        assert_eq!(agent.reasoning, Some(hya_provider::ReasoningEffort::Medium));
    }

    #[test]
    fn agent_with_model_omits_process_cwd_skill_index() {
        let home = tempdir();
        let workdir = tempdir();
        let _env = EnvGuard::set(&home, &workdir);
        write_skill(
            &workdir.join(".hya/skills/baseline"),
            "baseline-skill",
            "Baseline skill",
            "baseline body",
        );

        let agent = agent_with_model("fake", None);

        assert!(!agent.system_prompt.contains("Available skills"));
        assert!(
            !agent
                .system_prompt
                .contains("These skills are available on demand")
        );
        assert!(!agent.system_prompt.contains("baseline-skill"));
        assert!(!agent.system_prompt.contains("Baseline skill"));
    }

    /// Direct exec/RPC/goal construction still bakes Environment + AGENTS.
    #[test]
    fn agent_with_model_retains_environment_and_agents_context() {
        let home = tempdir();
        let workdir = tempdir();
        let _env = EnvGuard::set(&home, &workdir);
        let agents_marker = "DIRECT_MODE_AGENTS_CONTEXT_MARKER";
        std::fs::write(workdir.join("AGENTS.md"), agents_marker).unwrap();

        let agent = agent_with_model("fake", None);

        assert!(
            agent.system_prompt.contains(HARNESS_AGENT_BASE),
            "direct agent must keep harness base: {}",
            agent.system_prompt
        );
        assert!(
            agent.system_prompt.contains("## Environment"),
            "direct agent must bake Environment: {}",
            agent.system_prompt
        );
        assert!(
            agent.system_prompt.contains(agents_marker),
            "direct agent must bake process-cwd AGENTS: {}",
            agent.system_prompt
        );
        assert!(
            agent.system_prompt.contains("## Project context:"),
            "direct agent must use project-context separators: {}",
            agent.system_prompt
        );
    }

    /// Server/TUI AppState agent slot is base-only (no pre-baked AGENTS).
    #[test]
    fn agent_base_with_model_excludes_prebaked_agents_and_environment() {
        let home = tempdir();
        let workdir = tempdir();
        let _env = EnvGuard::set(&home, &workdir);
        let agents_marker = "SERVER_BASE_MUST_NOT_BAKE_AGENTS";
        std::fs::write(workdir.join("AGENTS.md"), agents_marker).unwrap();

        let agent = agent_base_with_model("fake", None);

        assert_eq!(agent.system_prompt, HARNESS_AGENT_BASE);
        assert!(
            !agent.system_prompt.contains("## Environment"),
            "server base must not bake Environment"
        );
        assert!(
            !agent.system_prompt.contains(agents_marker),
            "server base must not bake AGENTS (layered per turn)"
        );
        assert!(!agent.system_prompt.contains("## Project context:"));
    }

    /// Minimal engine whose catalog deliberately omits a recorded historical id.
    async fn engine_with_catalog(catalog: Arc<AgentCatalog>) -> SessionEngine {
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            catalog,
        ));
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
            runtime,
            permission,
            EventBus::default(),
        )
    }

    fn catalog_with_agents(stable_ids: &[&str]) -> Arc<AgentCatalog> {
        // One bundle per agent; built-in ids come from the compiled-in registry.
        let bundles = stable_ids
            .iter()
            .filter(|stable_id| !hya_core::is_builtin_id(stable_id))
            .map(|stable_id| PreparedBundle {
                format_version: 1,
                identity: BundleIdentity {
                    id: format!("hya/recovery-resolution-{stable_id}"),
                    version: "0.0.0".to_string(),
                    publisher: "hya-tests".to_string(),
                },
                digest: format!("test-only-{stable_id}"),
                agent: PreparedAgent {
                    id: AgentName::new(*stable_id),
                    description: None,
                    role: AgentRole::Main,
                    color: None,
                    prompt: Some(format!("{stable_id} recovery prompt")),
                    prompt_source: None,
                    prompt_digest: None,
                    model_policy: ModelPolicy::default(),
                    workdir: None,
                    spawn_lifecycle: SpawnLifecycle::Transient,
                    resource_view: ResourceView::default(),
                    // Deliberately empty: recovery must not depend on can_spawn.
                    can_spawn: Vec::new(),
                    hook_refs: Vec::new(),
                },
                tools: Vec::new(),
                skills: Vec::new(),
                mcp: Vec::new(),
                hooks: Vec::new(),
                extensions: Vec::new(),
            })
            .collect::<Vec<_>>();
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&bundles).expect("valid recovery catalog"),
        ))
    }

    #[tokio::test]
    async fn recovered_resident_missing_definition_fails_closed_without_synthesis() {
        let workdir = tempdir();
        let engine = engine_with_catalog(catalog_with_agents(&["build", "general"])).await;
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base-model"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let recorded = AgentName::new("legacy-resident-only");

        let err = match resolve_recovered_resident_agent(&engine, &base, &recorded, &workdir).await
        {
            Ok(_) => panic!("absent recorded id must fail typed definition missing"),
            Err(err) => err,
        };
        assert!(
            matches!(
                &err,
                CoreError::AgentDefinitionMissing { agent_id } if agent_id == "legacy-resident-only"
            ),
            "expected AgentDefinitionMissing for recorded id, got {err}"
        );
        assert!(
            err.to_string().contains("AGENT_DEFINITION_MISSING"),
            "typed surface must remain AGENT_DEFINITION_MISSING, got {err}"
        );
        // No general/base rewrite of the recorded identity.
        assert!(
            !err.to_string().contains("`general`") && !err.to_string().contains("`build`"),
            "error must name the recorded id only, got {err}"
        );
    }

    #[tokio::test]
    async fn recovered_resident_exact_lookup_preserves_recorded_id_without_can_spawn() {
        let workdir = tempdir();
        // Agent present for exact lookup but not in anyone's can_spawn (empty lists).
        let engine = engine_with_catalog(catalog_with_agents(&["build", "resident-helper"])).await;
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base-model"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let recorded = AgentName::new("resident-helper");

        let (_binding, resolved) =
            resolve_recovered_resident_agent(&engine, &base, &recorded, &workdir)
                .await
                .expect("exact catalog hit must resolve without can_spawn");
        assert_eq!(
            resolved.name.as_str(),
            "resident-helper",
            "recorded AgentName must remain stable"
        );
        assert_eq!(
            resolved.workdir, workdir,
            "session workdir from bind must be preserved"
        );
        assert!(
            resolved
                .system_prompt
                .contains("resident-helper recovery prompt"),
            "exact definition prompt must apply: {}",
            resolved.system_prompt
        );
        assert_ne!(
            resolved.name.as_str(),
            "general",
            "must not fall back to general"
        );
        assert_ne!(
            resolved.name.as_str(),
            "build",
            "must not rewrite to base/lead"
        );
    }

    #[tokio::test]
    async fn recovered_resident_uses_session_workdir_not_base_workdir() {
        let session_dir = tempdir();
        let base_dir = tempdir();
        let engine = engine_with_catalog(catalog_with_agents(&["build", "resident-helper"])).await;
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base-model"),
            system_prompt: "lead base".to_string(),
            workdir: base_dir.clone(),
            reasoning: None,
        };
        let recorded = AgentName::new("resident-helper");

        let (_binding, resolved) =
            resolve_recovered_resident_agent(&engine, &base, &recorded, &session_dir)
                .await
                .unwrap();
        assert_eq!(resolved.workdir, session_dir);
        assert_ne!(resolved.workdir, base_dir);
    }

    /// Catalog with one spawnable worker whose Bundle model_policy is explicit.
    fn catalog_with_worker_policy(model_policy: ModelPolicy) -> Arc<AgentCatalog> {
        // `build` is a compiled-in built-in; only `worker` needs a bundle.
        let bundle = |stable_id: &str, role: AgentRole, can_spawn: &[&str], policy: ModelPolicy| {
            let agent = PreparedAgent {
                id: AgentName::new(stable_id),
                description: None,
                role,
                color: None,
                prompt: Some(format!("{stable_id} prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: policy,
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                resource_view: ResourceView::default(),
                can_spawn: can_spawn.iter().map(|id| AgentName::new(*id)).collect(),
                hook_refs: Vec::new(),
            };
            PreparedBundle {
                format_version: 1,
                identity: BundleIdentity {
                    id: format!("hya/spawn-model-precedence-{stable_id}"),
                    version: "0.0.0".to_string(),
                    publisher: "hya-tests".to_string(),
                },
                digest: format!("test-only-{stable_id}"),
                agent,
                tools: Vec::new(),
                skills: Vec::new(),
                mcp: Vec::new(),
                hooks: Vec::new(),
                extensions: Vec::new(),
            }
        };
        let bundles = vec![bundle("worker", AgentRole::Subagent, &[], model_policy)];
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&bundles).expect("valid precedence bundle catalog"),
        ))
    }

    fn precedence_categories() -> CategoryRegistry {
        let mut entries = HashMap::new();
        entries.insert(
            "bundle-cat".to_string(),
            CategoryEntry::from_candidates(&["cat/bundle-model".to_string()]).unwrap(),
        );
        entries.insert(
            "inline-cat".to_string(),
            CategoryEntry::from_candidates(&["cat/inline-model".to_string()]).unwrap(),
        );
        entries.insert(
            "spawn-cat".to_string(),
            CategoryEntry::from_candidates(&["cat/spawn-model".to_string()]).unwrap(),
        );
        CategoryRegistry::from_entries(entries)
    }

    /// Highest-to-lowest spawn model chain, each row selecting the first set layer
    /// while lower layers remain present so the winner is unambiguous.
    #[tokio::test]
    async fn resolve_spawn_member_model_precedence_highest_to_lowest() {
        let workdir = tempdir();
        let categories = precedence_categories();
        let is_servable = |_: &ModelRef| true;
        let allowed = [AgentDef {
            name: "worker".to_string(),
            description: None,
            category: None,
            mode: "subagent".to_string(),
        }];
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base/model"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let sidecar_environment = BundleSidecarEnvironment::from_command(
            vec!["bun".to_string(), "sidecar.js".to_string()],
            tempdir(),
        );

        #[derive(Clone, Copy)]
        struct Case {
            label: &'static str,
            bundle_model: Option<&'static str>,
            bundle_category: Option<&'static str>,
            inline_model: Option<&'static str>,
            inline_category: Option<&'static str>,
            spawn_model: Option<&'static str>,
            spawn_category: Option<&'static str>,
            expected: &'static str,
        }

        // Cumulative lower layers prove exact order:
        // spawn model > spawn category > inline model > Bundle model >
        // inline category > Bundle category > base model.
        let cases = [
            Case {
                label: "base model",
                bundle_model: None,
                bundle_category: None,
                inline_model: None,
                inline_category: None,
                spawn_model: None,
                spawn_category: None,
                expected: "base/model",
            },
            Case {
                label: "Bundle category",
                bundle_model: None,
                bundle_category: Some("bundle-cat"),
                inline_model: None,
                inline_category: None,
                spawn_model: None,
                spawn_category: None,
                expected: "cat/bundle-model",
            },
            Case {
                label: "inline category over Bundle category",
                bundle_model: None,
                bundle_category: Some("bundle-cat"),
                inline_model: None,
                inline_category: Some("inline-cat"),
                spawn_model: None,
                spawn_category: None,
                expected: "cat/inline-model",
            },
            Case {
                label: "Bundle model over category layers",
                bundle_model: Some("bundle/model"),
                bundle_category: Some("bundle-cat"),
                inline_model: None,
                inline_category: Some("inline-cat"),
                spawn_model: None,
                spawn_category: None,
                expected: "bundle/model",
            },
            Case {
                label: "inline model over Bundle model",
                bundle_model: Some("bundle/model"),
                bundle_category: Some("bundle-cat"),
                inline_model: Some("inline/model"),
                inline_category: Some("inline-cat"),
                spawn_model: None,
                spawn_category: None,
                expected: "inline/model",
            },
            Case {
                label: "spawn category over inline model",
                bundle_model: Some("bundle/model"),
                bundle_category: Some("bundle-cat"),
                inline_model: Some("inline/model"),
                inline_category: Some("inline-cat"),
                spawn_model: None,
                spawn_category: Some("spawn-cat"),
                expected: "cat/spawn-model",
            },
            Case {
                label: "spawn explicit model highest",
                bundle_model: Some("bundle/model"),
                bundle_category: Some("bundle-cat"),
                inline_model: Some("inline/model"),
                inline_category: Some("inline-cat"),
                spawn_model: Some("spawn/model"),
                spawn_category: Some("spawn-cat"),
                expected: "spawn/model",
            },
        ];

        for case in cases {
            let catalog = catalog_with_worker_policy(ModelPolicy {
                model: case.bundle_model.map(str::to_string),
                category: case.bundle_category.map(str::to_string),
                reasoning: None,
            });
            let engine = engine_with_catalog(catalog).await;
            let binding = engine
                .bind_runtime(&workdir)
                .expect("bind prepared catalog");
            assert!(
                binding.resolve_agent("worker").is_some(),
                "{}: worker must resolve from prepared BundleCatalog",
                case.label
            );

            let has_inline = case.inline_model.is_some() || case.inline_category.is_some();
            let member = SpawnMember {
                description: case.label.to_string(),
                prompt: "resolve model only".to_string(),
                subagent_type: "worker".to_string(),
                model: case.spawn_model.map(str::to_string),
                category: case.spawn_category.map(str::to_string),
                inline_agent: has_inline.then(|| InlineAgent {
                    name: "overlay".to_string(),
                    prompt: "overlay prompt".to_string(),
                    model: case.inline_model.map(str::to_string),
                    category: case.inline_category.map(str::to_string),
                    ..InlineAgent::default()
                }),
                ..SpawnMember::default()
            };

            let resolve_ctx = ResolveSpawnMemberCtx {
                engine: &engine,
                binding: &binding,
                base: &base,
                caller: "build",
                allowed_agents: &allowed,
                categories: &categories,
                is_servable: &is_servable,
                guidance: None,
                sidecar_environment: &sidecar_environment,
            };
            let resolved = resolve_spawn_member(&resolve_ctx, member)
                .unwrap_or_else(|err| panic!("{}: resolve failed: {err}", case.label));

            assert_eq!(
                resolved.agent.model.as_str(),
                case.expected,
                "{}: expected model {}",
                case.label,
                case.expected
            );
            assert_eq!(
                resolved.authorized_target.as_str(),
                "worker",
                "{}: authorized target must remain the Bundle agent",
                case.label
            );
        }
    }

    #[tokio::test]
    async fn executable_spawn_resolution_captures_sidecar_factory_before_admission() {
        let workdir = tempdir();
        let engine = engine_with_catalog(Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_bundle("executable")])
                .expect("executable fixture catalog"),
        )))
        .await;
        let binding = engine.bind_runtime(&workdir).expect("bind executable turn");
        let staging_root = tempdir();
        let environment = BundleSidecarEnvironment::from_command(
            vec!["bun".to_string(), "sidecar.js".to_string()],
            staging_root,
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base/model"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let allowed = [AgentDef {
            name: "worker".to_string(),
            description: None,
            category: None,
            mode: "subagent".to_string(),
        }];
        let categories = CategoryRegistry::default();
        let is_servable = |_: &ModelRef| true;
        let member = SpawnMember {
            prompt: "sidecar pre-admission".to_string(),
            subagent_type: "worker".to_string(),
            ..SpawnMember::default()
        };
        let resolved = resolve_spawn_member(
            &ResolveSpawnMemberCtx {
                engine: &engine,
                binding: &binding,
                base: &base,
                caller: "build",
                allowed_agents: &allowed,
                categories: &categories,
                is_servable: &is_servable,
                guidance: None,
                sidecar_environment: &environment,
            },
            member,
        )
        .expect("authorized executable worker must resolve");
        assert!(
            resolved.sidecar_factory.is_some(),
            "executable Bundle must capture its sidecar factory before admission"
        );
    }

    fn activation_sidecar_fixture(hooks: &str) -> String {
        r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "compat"},
            "hooks": __HOOKS__,
            "tools": [{"name": "echo", "description": "sidecar echo", "inputSchema": {"type": "object"}}],
            "workspaceAdapters": []
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif method == "shutdown":
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": True}), flush=True)
        break
"#
        .replace("__HOOKS__", hooks)
    }

    #[tokio::test]
    async fn bundle_sidecar_factory_exposes_declared_activation_hooks_after_ack() {
        let mut bundle = materialized_bundle("activation-hooks");
        let event_hook_id = bundle.hooks[0].stable_id.clone();
        let before_hook = materialized_resource(
            "activation-hooks",
            "hook",
            "tool.execute.before",
            "extensions/runtime.js",
        );
        let after_hook = materialized_resource(
            "activation-hooks",
            "hook",
            "tool.execute.after",
            "extensions/runtime.js",
        );
        bundle.agent.hook_refs = vec![
            event_hook_id,
            before_hook.stable_id.clone(),
            after_hook.stable_id.clone(),
        ];
        bundle.hooks.extend([before_hook, after_hook]);
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[bundle]).expect("activation hook fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture activation hook turn binding");
        let staging_root = tempdir();
        let fixture = activation_sidecar_fixture(
            r#"[
                {"name": "tool.execute.before", "posture": "safe"},
                {"name": "tool.execute.after", "posture": "open"},
                {"name": "event"}
            ]"#,
        );
        let environment = BundleSidecarEnvironment::from_command(
            vec!["python3".to_string(), "-c".to_string(), fixture.to_string()],
            staging_root.clone(),
        );
        let factory = environment
            .factory_for(&binding, "worker")
            .expect("resolve activation hook sidecar factory")
            .expect("materialized Bundle must expose its sidecar factory");
        let mut handle = factory
            .start(SidecarStart {
                activation_id: "activation-hooks-red".to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await
            .expect("start transient activation");
        handle
            .ready()
            .await
            .expect("activation must acknowledge ready");
        assert!(
            handle.hook_dispatcher().is_some(),
            "declared activation hooks must be exposed after ACK"
        );

        drop(handle);
        std::fs::remove_dir_all(staging_root).expect("cleanup activation staging root");
        std::fs::remove_dir_all(turn_dir).expect("cleanup activation turn directory");
    }

    #[tokio::test]
    async fn bundle_sidecar_hook_declarations_match_captured_selected_set() {
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[disjoint_materialized_bundle("selected-hooks")])
                .expect("selected-hooks fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture selected-hooks turn binding");
        let staging_root = tempdir();

        assert!(matches!(
            validate_bundle_sidecar_hooks(&binding, "alpha", &[]),
            Err(CoreError::Invalid(_))
        ));

        let selected_fixture = activation_sidecar_fixture(r#"[{"name": "event"}]"#);
        let selected_environment = BundleSidecarEnvironment::from_command(
            vec!["python3".to_string(), "-c".to_string(), selected_fixture],
            staging_root.clone(),
        );
        let selected_factory = selected_environment
            .factory_for(&binding, "alpha")
            .expect("resolve selected-hooks sidecar factory")
            .expect("selected alpha capabilities must expose a sidecar factory");
        let mut selected_handle = selected_factory
            .start(SidecarStart {
                activation_id: "activation-selected-hooks".to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await
            .expect("selected hook declaration must start");
        selected_handle
            .ready()
            .await
            .expect("selected hook declaration must acknowledge ready");
        selected_handle
            .shutdown()
            .await
            .expect("selected hook declaration must shut down cleanly");

        let extra_fixture = activation_sidecar_fixture(
            r#"[
                {"name": "event"},
                {"name": "tool.execute.before"}
            ]"#,
        );
        let extra_environment = BundleSidecarEnvironment::from_command(
            vec!["python3".to_string(), "-c".to_string(), extra_fixture],
            staging_root.clone(),
        );
        let extra_factory = extra_environment
            .factory_for(&binding, "alpha")
            .expect("resolve extra-hook sidecar factory")
            .expect("selected alpha capabilities must expose a sidecar factory");
        let extra_result = extra_factory
            .start(SidecarStart {
                activation_id: "activation-extra-hook".to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await;
        let rejected = match extra_result {
            Err(CoreError::Invalid(_)) => true,
            Ok(mut handle) => {
                let _ = handle.shutdown().await;
                false
            }
            Err(error) => panic!("unexpected extra hook declaration error: {error}"),
        };

        std::fs::remove_dir_all(&staging_root).expect("cleanup selected-hooks staging root");
        std::fs::remove_dir_all(&turn_dir).expect("cleanup selected-hooks turn directory");

        assert!(
            rejected,
            "unselected hook declaration must be rejected before activation"
        );
    }

    #[tokio::test]
    async fn bundle_sidecar_handle_shutdown_reaps_child_and_removes_activation_staging() {
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_tool_bundle("shutdown")])
                .expect("shutdown fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture shutdown turn binding");
        let staging_root = tempdir();
        let fixture = r#"
import json, os, sys
activation_dir = os.getcwd()
sentinel = os.path.join(os.path.dirname(activation_dir), "shutdown.sentinel")
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "compat"},
            "hooks": [],
            "tools": [{"name": "echo", "description": "sidecar echo", "inputSchema": {"type": "object"}}],
            "workspaceAdapters": []
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif method == "shutdown":
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": {}}), flush=True)
        with open(sentinel, "w", encoding="utf-8") as handle:
            handle.write("shutdown")
        break
"#;
        let environment = BundleSidecarEnvironment::from_command(
            vec!["python3".to_string(), "-c".to_string(), fixture.to_string()],
            staging_root.clone(),
        );
        let factory = environment
            .factory_for(&binding, "worker")
            .expect("resolve shutdown sidecar factory")
            .expect("materialized Bundle must expose a sidecar factory");
        let activation_id = "activation-shutdown";
        let mut handle = factory
            .start(SidecarStart {
                activation_id: activation_id.to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await
            .expect("start transient shutdown activation");
        handle
            .ready()
            .await
            .expect("shutdown activation must acknowledge ready");

        handle.shutdown().await.expect("shutdown must reap child");
        assert_eq!(
            std::fs::read_to_string(staging_root.join("shutdown.sentinel"))
                .expect("shutdown fixture sentinel"),
            "shutdown"
        );
        assert!(
            !staging_root.join(activation_id).exists(),
            "activation staging must be removed after shutdown"
        );

        std::fs::remove_dir_all(staging_root).expect("cleanup shutdown staging root");
        std::fs::remove_dir_all(turn_dir).expect("cleanup shutdown turn directory");
    }

    #[tokio::test]
    async fn bundle_sidecar_handle_drop_removes_activation_staging() {
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_tool_bundle("drop-cleanup")])
                .expect("drop cleanup fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture drop cleanup turn binding");
        let staging_root = tempdir();
        let fixture = activation_sidecar_fixture("[]");
        let environment = BundleSidecarEnvironment::from_command(
            vec!["python3".to_string(), "-c".to_string(), fixture],
            staging_root.clone(),
        );
        let factory = environment
            .factory_for(&binding, "worker")
            .expect("resolve drop cleanup sidecar factory")
            .expect("materialized Bundle must expose its sidecar factory");
        let activation_id = "activation-drop-cleanup";
        let activation_dir = staging_root.join(activation_id);
        let mut handle = factory
            .start(SidecarStart {
                activation_id: activation_id.to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await
            .expect("start transient drop cleanup activation");
        handle
            .ready()
            .await
            .expect("drop cleanup activation must acknowledge ready");
        assert!(
            activation_dir.is_dir(),
            "sidecar start must create its unique activation directory"
        );

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(handle);
            let activation_removed = !activation_dir.exists();
            let parent_empty_or_removed = match std::fs::read_dir(&staging_root) {
                Ok(mut entries) => entries.next().is_none(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => panic!(
                    "read activation staging root `{}` after handle drop: {error}",
                    staging_root.display()
                ),
            };
            assert!(
                activation_removed && parent_empty_or_removed,
                "dropping BundleSidecarHandle must remove its activation directory and leave the staging parent empty"
            );
        }));

        std::fs::remove_dir_all(&staging_root).expect("cleanup drop cleanup staging root");
        std::fs::remove_dir_all(&turn_dir).expect("cleanup drop cleanup turn directory");
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bundle_sidecar_factory_start_cancellation_removes_activation_staging() {
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_tool_bundle("cancel-start")])
                .expect("cancel-start fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture cancel-start turn binding");
        let staging_root = tempdir();
        let environment = BundleSidecarEnvironment::from_command(
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf started > sidecar-started.marker; cat >/dev/null".to_string(),
            ],
            staging_root.clone(),
        );
        let factory = environment
            .factory_for(&binding, "worker")
            .expect("resolve cancel-start sidecar factory")
            .expect("materialized Bundle must expose its sidecar factory");
        let activation_id = "activation-cancel-start";
        let activation_dir = staging_root.join(activation_id);
        let marker = activation_dir.join("sidecar-started.marker");
        let start_factory = factory.clone();
        let start = tokio::spawn(async move {
            start_factory
                .start(SidecarStart {
                    activation_id: activation_id.to_string(),
                    lifecycle: SidecarLifecycle::Transient,
                })
                .await
        });

        let marker_written = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if marker.is_file() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();
        let activation_created = activation_dir.is_dir();
        let initialize_pending = !start.is_finished();
        start.abort();
        let join_cancelled = match start.await {
            Ok(_) => false,
            Err(error) => error.is_cancelled(),
        };
        let activation_removed = !activation_dir.exists();
        let staging_empty = match std::fs::read_dir(&staging_root) {
            Ok(mut entries) => entries.next().is_none(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        };
        let _ = std::fs::remove_dir_all(&staging_root);
        let _ = std::fs::remove_dir_all(&turn_dir);

        assert!(
            marker_written,
            "sidecar child must write its activation marker"
        );
        assert!(
            activation_created,
            "sidecar start must create its activation directory"
        );
        assert!(
            initialize_pending,
            "sidecar initialize must remain pending before cancellation"
        );
        assert!(
            join_cancelled,
            "aborting pending factory start must cancel its task"
        );
        assert!(
            activation_removed,
            "cancelling sidecar start must remove its activation directory"
        );
        assert!(
            staging_empty,
            "cancelling sidecar start must leave staging root empty"
        );
    }

    #[tokio::test]
    async fn bundle_sidecar_handle_exposes_transport_loss_token() {
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_tool_bundle("loss-token")])
                .expect("loss token fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture loss token turn binding");
        let staging_root = tempdir();
        let fixture = activation_sidecar_fixture("[]");
        let environment = BundleSidecarEnvironment::from_command(
            vec!["python3".to_string(), "-c".to_string(), fixture],
            staging_root.clone(),
        );
        let factory = environment
            .factory_for(&binding, "worker")
            .expect("resolve loss token sidecar factory")
            .expect("materialized Bundle must expose its sidecar factory");
        let activation_id = "activation-loss-token";
        let activation_dir = staging_root.join(activation_id);
        let mut handle = factory
            .start(SidecarStart {
                activation_id: activation_id.to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await
            .expect("start transient loss token activation");
        handle
            .ready()
            .await
            .expect("loss token activation must acknowledge ready");

        let activation_created = activation_dir.is_dir();
        let loss_token = handle.loss_token();
        handle
            .terminate()
            .await
            .expect("terminate loss token activation");
        let activation_removed = !activation_dir.exists();
        std::fs::remove_dir_all(&staging_root).expect("cleanup loss token staging root");
        std::fs::remove_dir_all(&turn_dir).expect("cleanup loss token turn directory");

        assert!(
            activation_created,
            "sidecar start must create its unique activation directory"
        );
        assert!(
            activation_removed,
            "explicit sidecar termination must remove activation staging"
        );
        assert!(
            loss_token.is_some(),
            "BundleSidecarHandle must expose a transport loss token"
        );
    }

    #[tokio::test]
    async fn directory_bundle_importing_undeclared_authoring_helper_fails_before_ack() {
        let authoring_root = tempdir();
        let staging_root = tempdir();
        let turn_dir = tempdir();
        std::fs::create_dir_all(authoring_root.join("extensions")).unwrap();
        std::fs::write(
            authoring_root.join("bundle.hya.md"),
            br#"---
kind: AgentBundle
identity:
  id: hya/directory-helper-import
  version: 0.0.1
  publisher: hya-tests
resources:
  tools:
    - id: echo
      path: extensions/main.js
extensions:
  js:
    - id: main
      path: extensions/main.js
agent:
  id: directory-helper-main
  role: main
  spawn_lifecycle: transient
  resource_view:
    allow:
      - echo
---
You are a directory-authored executable Bundle agent.
"#,
        )
        .unwrap();
        std::fs::write(
            authoring_root.join("extensions/main.js"),
            br#"import "./helper.js";

export default {
  id: "directory-helper-import",
  server: async () => ({
    tool: {
      echo: {
        description: "echo",
        execute: async () => "ok",
      },
    },
  }),
};
"#,
        )
        .unwrap();
        std::fs::write(
            authoring_root.join("extensions/helper.js"),
            "export const helper = true;\n",
        )
        .unwrap();

        let source = BundleSource::read_directory(&authoring_root)
            .expect("read directory-authored Bundle source");
        let prepared = prepare_package(source).expect("prepare directory-authored Bundle");
        assert_eq!(prepared.bundles()[0].extensions.len(), 1);
        assert_eq!(
            prepared.bundles()[0].extensions[0].source_path,
            "extensions/main.js"
        );
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(prepared.bundles())
                .expect("build directory-authored Bundle catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture directory-authored Bundle turn binding");
        let command = plugins::bundle_sidecar_command().expect("Bun must be available");
        let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
        let factory = environment
            .factory_for(&binding, "directory-helper-main")
            .expect("resolve directory-authored sidecar factory")
            .expect("selected tool must expose a sidecar factory");
        let activation_id = "directory-helper-import";
        let activation_dir = staging_root.join(activation_id);
        assert!(
            authoring_root.join("extensions/helper.js").is_file(),
            "authoring-only helper must remain present beside the source entrypoint"
        );

        let outcome = factory
            .start(SidecarStart {
                activation_id: activation_id.to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await;
        let detail = match outcome {
            Err(CoreError::Invalid(detail)) => detail,
            Ok(mut handle) => {
                let _ = handle.shutdown().await;
                let _ = std::fs::remove_dir_all(&authoring_root);
                let _ = std::fs::remove_dir_all(&staging_root);
                let _ = std::fs::remove_dir_all(&turn_dir);
                panic!("authoring-only helper import unexpectedly reached ACK");
            }
            Err(error) => {
                let _ = std::fs::remove_dir_all(&authoring_root);
                let _ = std::fs::remove_dir_all(&staging_root);
                let _ = std::fs::remove_dir_all(&turn_dir);
                panic!("unexpected sidecar startup error: {error}");
            }
        };
        assert!(
            !detail.is_empty(),
            "pre-ACK helper import failure must carry a typed diagnostic"
        );
        let authoring_helper_present = authoring_root.join("extensions/helper.js").is_file();
        let activation_removed = !activation_dir.exists();
        let staging_empty = std::fs::read_dir(&staging_root).unwrap().next().is_none();
        std::fs::remove_dir_all(&authoring_root).unwrap();
        std::fs::remove_dir_all(&staging_root).unwrap();
        std::fs::remove_dir_all(&turn_dir).unwrap();

        assert!(
            authoring_helper_present,
            "the source-tree helper must still exist when isolated activation fails"
        );
        assert!(
            activation_removed,
            "failed helper import must remove the activation staging directory"
        );
        assert!(
            staging_empty,
            "failed helper import must leave the staging root empty"
        );
    }

    #[tokio::test]
    async fn bundle_sidecar_factory_passes_materialized_extension_to_bun() {
        let bundle = materialized_bun_bundle("bun-extension");
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[bundle]).expect("Bun extension fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture Bun extension turn binding");
        let staging_root = tempdir();
        let command = plugins::bundle_sidecar_command().expect("Bun must be available");
        let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
        let factory = environment
            .factory_for(&binding, "worker")
            .expect("resolve Bun extension sidecar factory")
            .expect("materialized Bundle must expose a sidecar factory");
        let activation_id = "activation-bun-extension";
        let start = factory
            .start(SidecarStart {
                activation_id: activation_id.to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await;
        let mut handle = match start {
            Ok(handle) => handle,
            Err(error) => {
                std::fs::remove_dir_all(staging_root).expect("cleanup Bun staging root");
                std::fs::remove_dir_all(turn_dir).expect("cleanup Bun turn directory");
                panic!("start Bun extension sidecar: {error}");
            }
        };
        handle
            .ready()
            .await
            .expect("Bun extension activation must acknowledge ready");
        let tool_names = handle
            .tool_bindings()
            .iter()
            .map(|binding| binding.tool.name().to_string())
            .collect::<Vec<_>>();

        handle
            .shutdown()
            .await
            .expect("shutdown Bun extension sidecar");
        assert!(
            !staging_root.join(activation_id).exists(),
            "activation staging must be removed after shutdown"
        );
        std::fs::remove_dir_all(staging_root).expect("cleanup Bun staging root");
        std::fs::remove_dir_all(turn_dir).expect("cleanup Bun turn directory");
        assert_eq!(
            tool_names,
            vec!["bundle:hya/materialized/tool/echo"],
            "Bun extension must expose exactly the materialized echo tool"
        );
    }

    #[tokio::test]
    async fn bun_sidecars_load_only_each_agent_captured_entrypoint() {
        let mut bundles = disjoint_materialized_bundles("bun-disjoint");
        for bundle in &mut bundles {
            set_materialized_extension_content_for_path(
                bundle,
                "extensions/alpha.js",
                r#"
export default {
  id: "alpha-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("alpha extension received unexpected initialization input")
    }
    return {
      tool: {
        echo: {
          description: "alpha echo",
          execute: async () => "alpha",
        },
      },
      event: async () => {},
    }
  },
}
"#
                .to_string(),
            );
            set_materialized_extension_content_for_path(
                bundle,
                "extensions/beta.js",
                r#"
export default {
  id: "beta-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("beta extension received unexpected initialization input")
    }
    return {
      tool: {
        beta: {
          description: "beta tool",
          execute: async () => "beta",
        },
      },
      "tool.execute.before": async () => {},
    }
  },
}
"#
                .to_string(),
            );
        }

        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&bundles)
                .expect("disjoint Bun entrypoint fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), Arc::clone(&catalog));
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture disjoint Bun entrypoint turn binding");
        let command = plugins::bundle_sidecar_command().expect("Bun must be available");
        let staging_root = tempdir();
        let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
        let alpha_factory = environment
            .factory_for(&binding, "alpha")
            .expect("resolve alpha Bun sidecar factory")
            .expect("alpha must expose a Bun sidecar factory");
        let beta_factory = environment
            .factory_for(&binding, "beta")
            .expect("resolve beta Bun sidecar factory")
            .expect("beta must expose a Bun sidecar factory");
        let alpha_activation = "bun-disjoint-alpha";
        let beta_activation = "bun-disjoint-beta";
        let mut alpha_handle = alpha_factory
            .start(SidecarStart {
                activation_id: alpha_activation.to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await
            .expect("start alpha Bun sidecar");
        alpha_handle
            .ready()
            .await
            .expect("alpha Bun sidecar must acknowledge ready");
        let mut beta_handle = beta_factory
            .start(SidecarStart {
                activation_id: beta_activation.to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await
            .expect("start beta Bun sidecar");
        beta_handle
            .ready()
            .await
            .expect("beta Bun sidecar must acknowledge ready");

        let alpha_tools = alpha_handle
            .tool_bindings()
            .iter()
            .map(|binding| binding.tool.name().to_string())
            .collect::<Vec<_>>();
        let beta_tools = beta_handle
            .tool_bindings()
            .iter()
            .map(|binding| binding.tool.name().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            alpha_tools,
            vec!["bundle:hya/materialized/tool/echo"],
            "alpha must bind only its selected tool"
        );
        assert_eq!(
            beta_tools,
            vec!["bundle:hya/materialized-beta/tool/beta"],
            "beta must bind only its selected tool"
        );
        assert!(alpha_handle.hook_dispatcher().is_some());
        assert!(beta_handle.hook_dispatcher().is_some());

        let alpha_dir = staging_root.join(alpha_activation);
        let beta_dir = staging_root.join(beta_activation);
        assert!(alpha_dir.join("extensions/alpha.js").is_file());
        assert!(!alpha_dir.join("extensions/beta.js").exists());
        assert!(beta_dir.join("extensions/beta.js").is_file());
        assert!(!beta_dir.join("extensions/alpha.js").exists());

        alpha_handle
            .shutdown()
            .await
            .expect("shutdown alpha Bun sidecar");
        beta_handle
            .shutdown()
            .await
            .expect("shutdown beta Bun sidecar");
        assert!(!alpha_dir.exists());
        assert!(!beta_dir.exists());
        assert!(
            std::fs::read_dir(&staging_root)
                .expect("read disjoint Bun staging root")
                .next()
                .is_none(),
            "disjoint sidecar shutdown must leave staging root empty"
        );

        std::fs::remove_dir_all(&staging_root).expect("cleanup disjoint Bun staging root");
        std::fs::remove_dir_all(&turn_dir).expect("cleanup disjoint Bun turn directory");
        assert!(!staging_root.exists());
        assert!(!turn_dir.exists());
    }

    #[tokio::test]
    async fn bun_sidecar_rejects_generic_superset_module_before_activation() {
        let mut bundle = disjoint_materialized_bundle("bun-generic-superset");
        set_materialized_extension_content_for_path(
            &mut bundle,
            "extensions/alpha.js",
            r#"
export default {
  id: "alpha-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("alpha extension received unexpected initialization input")
    }
    return {
      tool: {
        echo: {
          description: "alpha echo",
          execute: async () => "alpha",
        },
        beta: {
          description: "unselected beta",
          execute: async () => "beta",
        },
      },
      event: async () => {},
    }
  },
}
"#
            .to_string(),
        );

        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[bundle]).expect("generic Bun superset fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), Arc::clone(&catalog));
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture generic Bun superset turn binding");
        let command = plugins::bundle_sidecar_command().expect("Bun must be available");
        let staging_root = tempdir();
        let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
        let factory = environment
            .factory_for(&binding, "alpha")
            .expect("resolve generic Bun superset sidecar factory")
            .expect("alpha must expose a Bun sidecar factory");
        let activation_id = "bun-generic-superset";
        let outcome = factory
            .start(SidecarStart {
                activation_id: activation_id.to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await;
        let rejected = match outcome {
            Err(CoreError::Invalid(_)) => true,
            Ok(mut handle) => {
                let _ = handle.shutdown().await;
                false
            }
            Err(error) => panic!("unexpected generic Bun superset error: {error}"),
        };
        assert!(
            rejected,
            "generic superset declarations must fail before activation"
        );
        assert!(
            !staging_root.join(activation_id).exists(),
            "rejected activation must remove its activation directory"
        );
        assert!(
            std::fs::read_dir(&staging_root)
                .expect("read generic Bun superset staging root")
                .next()
                .is_none(),
            "rejected activation must leave staging root empty"
        );

        std::fs::remove_dir_all(&staging_root).expect("cleanup generic Bun staging root");
        std::fs::remove_dir_all(&turn_dir).expect("cleanup generic Bun turn directory");
    }

    #[tokio::test]
    async fn transient_bun_bundle_runs_harness_tool_loop_and_reaps_sidecar() {
        let canonical = "bundle:hya/materialized/tool/echo";
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_bun_bundle("bun-e2e")])
                .expect("Bun E2E fixture catalog"),
        ));
        let provider = FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "echo".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("bundle complete".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ]);
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
            Action::Tool,
            canonical,
            Mode::Allow,
        )]));
        let engine = Arc::new(SessionEngine::new(
            SessionStore::connect_memory()
                .await
                .expect("connect E2E store"),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog)),
            permission,
            EventBus::default(),
        ));
        let workdir = tempdir();
        let lead = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .expect("create E2E lead");
        let binding = engine.bind_runtime(&workdir).expect("capture E2E binding");
        let staging_root = tempdir();
        let command = plugins::bundle_sidecar_command().expect("Bun must be available");
        let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let allowed = [AgentDef {
            name: "worker".to_string(),
            description: None,
            category: None,
            mode: "subagent".to_string(),
        }];
        let categories = CategoryRegistry::default();
        let is_servable = |_: &ModelRef| true;
        let resolved = resolve_spawn_member(
            &ResolveSpawnMemberCtx {
                engine: &engine,
                binding: &binding,
                base: &base,
                caller: "build",
                allowed_agents: &allowed,
                categories: &categories,
                is_servable: &is_servable,
                guidance: None,
                sidecar_environment: &environment,
            },
            SpawnMember {
                description: "bun e2e".to_string(),
                prompt: "run bundle tool".to_string(),
                subagent_type: "worker".to_string(),
                ..SpawnMember::default()
            },
        )
        .expect("resolve Bun E2E spawn member");
        let ResolvedSpawnMember {
            request,
            agent,
            binding,
            agents,
            resources,
            guidance,
            sidecar_factory,
            ..
        } = resolved;
        let spec = MemberSpec {
            id: MemberId::new(),
            agent,
            binding,
            agents,
            resources: Some(resources),
            guidance,
            directive: request.prompt,
            description: request.description,
            session: None,
            sidecar_factory,
            tool_call: None,
        };

        let evidence = run_team(engine.clone(), lead, vec![spec], Default::default()).await;
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].status, MemberStatus::Done);
        assert_eq!(evidence[0].summary, "bundle complete");

        let lead_projection = engine
            .read_projection(lead)
            .await
            .expect("read E2E lead projection");
        let child = lead_projection.session.members[0]
            .child
            .expect("E2E member child session");
        let lead_events = engine.store().replay(lead).await.expect("replay E2E lead");
        let finished = lead_events
            .iter()
            .filter(|envelope| {
                matches!(
                    &envelope.event,
                    Event::MemberFinished {
                        status: MemberRunStatus::Done,
                        child: Some(event_child),
                        ..
                    } if *event_child == child
                )
            })
            .count();
        assert_eq!(finished, 1);

        let child_events = engine
            .store()
            .replay(child)
            .await
            .expect("replay E2E child");
        assert!(child_events.iter().any(|envelope| {
            matches!(
                &envelope.event,
                Event::ToolResult { output, .. }
                    if output == &json!({"title": "", "output": "bundle-e2e", "metadata": {}})
            )
        }));
        assert!(
            !child_events
                .iter()
                .any(|envelope| matches!(&envelope.event, Event::ToolError { .. }))
        );
        assert!(
            std::fs::read_dir(&staging_root)
                .expect("read E2E staging root")
                .next()
                .is_none(),
            "transient sidecar shutdown must remove its activation directory"
        );

        std::fs::remove_dir_all(staging_root).expect("cleanup E2E staging root");
        std::fs::remove_dir_all(workdir).expect("cleanup E2E workdir");
    }

    #[tokio::test]
    async fn selected_main_bun_bundle_runs_harness_tool_hook_event_loop_and_reaps_sidecar() {
        let canonical = "bundle:hya/materialized/tool/echo";
        let mut bundle = materialized_bun_bundle("bun-root-hooks");
        let event_hook =
            materialized_resource("bun-root-hooks", "hook", "event", "extensions/runtime.js");
        let before_hook = materialized_resource(
            "bun-root-hooks",
            "hook",
            "tool.execute.before",
            "extensions/runtime.js",
        );
        let after_hook = materialized_resource(
            "bun-root-hooks",
            "hook",
            "tool.execute.after",
            "extensions/runtime.js",
        );
        bundle.agent.hook_refs = vec![
            event_hook.stable_id.clone(),
            before_hook.stable_id.clone(),
            after_hook.stable_id.clone(),
        ];
        bundle.hooks.extend([event_hook, before_hook, after_hook]);
        bundle.agent.id = AgentName::new("root-hook-agent");
        bundle.agent.role = AgentRole::Main;
        bundle.agent.spawn_lifecycle = SpawnLifecycle::Transient;
        set_materialized_extension_content(
            &mut bundle,
            r#"
let eventCount = 0
export default {
  id: "bundle-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("bundle extension received unexpected initialization input")
    }
    return {
      event: () => {
        eventCount += 1
      },
      "tool.execute.before": async (_input, output) => {
        if (eventCount <= 0 || output.args === null || typeof output.args !== "object") {
          throw new Error("bundle hook received unexpected tool input")
        }
        output.args.hooked = true
        output.args.eventCount = eventCount
      },
      "tool.execute.after": async (_input, output) => {
        output.output = `${output.output}:after`
      },
      tool: {
        echo: {
          description: "bundle echo",
          execute: async (input) => {
            if (
              input === null ||
              typeof input !== "object" ||
              input.hooked !== true ||
              !Number.isInteger(input.eventCount) ||
              input.eventCount <= 0
            ) {
              throw new Error("bundle echo did not receive hook mutation")
            }
            return `bun-root-hooks:${input.eventCount}`
          },
        },
      },
    }
  },
}
"#
            .to_string(),
        );
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[bundle]).expect("root Bun hook fixture catalog"),
        ));
        let provider = FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "echo".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("bundle root complete".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ]);
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
            Action::Tool,
            canonical,
            Mode::Allow,
        )]));
        let workdir = tempdir();
        let staging_root = tempdir();
        let command = plugins::bundle_sidecar_command().expect("Bun must be available");
        let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
        let engine = Arc::new(
            SessionEngine::new(
                SessionStore::connect_memory()
                    .await
                    .expect("connect root Bun hook store"),
                Arc::new(ProviderRouter::new().with(Arc::new(provider))),
                Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog)),
                permission,
                EventBus::default(),
            )
            .with_sidecar_environment(Arc::new(environment)),
        );
        let session = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("root-hook-agent"),
                model: ModelRef::new("fake"),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .expect("create root Bun hook session");
        engine
            .admit_user_prompt(session, "run root bundle hook".to_string())
            .await
            .expect("admit root Bun hook prompt");
        let finish = engine
            .run_turn(
                session,
                &AgentSpec {
                    name: AgentName::new("root-hook-agent"),
                    model: ModelRef::new("fake"),
                    system_prompt: "root base".to_string(),
                    workdir: workdir.clone(),
                    reasoning: None,
                },
                CancellationToken::new(),
            )
            .await
            .expect("run root Bun hook turn");
        assert_eq!(finish, FinishReason::Stop);

        let events = engine
            .store()
            .replay(session)
            .await
            .expect("replay root Bun hook turn");
        let successful_results = events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::ToolResult { output, .. } => Some(output.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(successful_results.len(), 1);
        let output = successful_results[0]
            .get("output")
            .and_then(Value::as_str)
            .expect("root Bun hook tool result output must be a string");
        assert!(output.starts_with("bun-root-hooks:"));
        assert!(output.ends_with(":after"));
        assert!(
            !events
                .iter()
                .any(|envelope| matches!(&envelope.event, Event::ToolError { .. }))
        );
        assert!(
            std::fs::read_dir(&staging_root)
                .expect("read root Bun hook staging root")
                .next()
                .is_none(),
            "transient root sidecar shutdown must remove its activation directory"
        );

        std::fs::remove_dir_all(staging_root).expect("cleanup root Bun hook staging root");
        std::fs::remove_dir_all(workdir).expect("cleanup root Bun hook workdir");
    }

    #[tokio::test]
    async fn resident_bun_bundle_reuses_one_sidecar_across_two_mailbox_turns() {
        let canonical = "bundle:hya/materialized/tool/echo";
        let mut bundle = materialized_bun_bundle("bun-resident");
        bundle.agent.spawn_lifecycle = SpawnLifecycle::Resident;
        set_materialized_extension_content(
            &mut bundle,
            r#"
let calls = 0
export default {
  id: "bundle-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("bundle extension received unexpected initialization input")
    }
    return {
      tool: {
        echo: {
          description: "bundle echo",
          execute: async (input) => {
            if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
              throw new Error("bundle echo received unexpected tool input")
            }
            return `${process.pid}:${++calls}`
          },
        },
      },
    }
  },
}
"#
            .to_string(),
        );
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[bundle]).expect("resident Bun fixture catalog"),
        ));
        let provider = FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "echo".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("resident first complete".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![
                FakeStep::ToolCall {
                    name: "echo".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("resident second complete".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ]);
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
            Action::Tool,
            canonical,
            Mode::Allow,
        )]));
        let engine = Arc::new(SessionEngine::new(
            SessionStore::connect_memory()
                .await
                .expect("connect resident E2E store"),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog)),
            permission,
            EventBus::default(),
        ));
        let workdir = tempdir();
        let lead = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .expect("create resident E2E lead");
        let binding = engine
            .bind_runtime(&workdir)
            .expect("capture resident E2E binding");
        let staging_root = tempdir();
        let command = plugins::bundle_sidecar_command().expect("Bun must be available");
        let environment = BundleSidecarEnvironment::from_command(command, staging_root.clone());
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let allowed = [AgentDef {
            name: "worker".to_string(),
            description: None,
            category: None,
            mode: "subagent".to_string(),
        }];
        let categories = CategoryRegistry::default();
        let is_servable = |_: &ModelRef| true;
        let resolved = resolve_spawn_member(
            &ResolveSpawnMemberCtx {
                engine: &engine,
                binding: &binding,
                base: &base,
                caller: "build",
                allowed_agents: &allowed,
                categories: &categories,
                is_servable: &is_servable,
                guidance: None,
                sidecar_environment: &environment,
            },
            SpawnMember {
                subagent_type: "worker".to_string(),
                resident: true,
                ..SpawnMember::default()
            },
        )
        .expect("resolve resident Bun spawn member");
        assert!(
            resolved.resident,
            "resident fixture must resolve as resident"
        );
        let ResolvedSpawnMember {
            agent,
            binding,
            agents,
            resources,
            sidecar_factory,
            ..
        } = resolved;
        let sidecar_factory = sidecar_factory.expect("resident Bun must capture sidecar factory");
        let supervisor = ResidentSupervisor::start(engine.clone());
        let (child, handle) = supervisor
            .spawn_resident(
                lead,
                agent,
                (binding, agents, resources, Some(sidecar_factory)),
                String::new(),
                None,
                None,
            )
            .await
            .expect("spawn resident Bun member");
        let mut bus = engine.bus().subscribe();

        engine
            .mail_send(
                lead,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "resident mail one".to_string(),
            )
            .await
            .expect("send first resident mail");
        loop {
            let envelope = bus.recv().await.expect("resident bus remains open");
            if matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    handle: event_handle,
                    status: RosterStatus::Idle,
                    ..
                } if event_handle == &handle
            ) {
                break;
            }
        }
        let first_projection = engine
            .read_projection(lead)
            .await
            .expect("read first resident roster");
        assert_eq!(
            first_projection
                .team
                .roster
                .get(&handle)
                .expect("resident roster entry")
                .status,
            RosterStatus::Idle
        );
        let first_events = engine
            .store()
            .replay(child)
            .await
            .expect("replay first resident child");
        let first_outputs = first_events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::ToolResult { output, .. } => Some(output.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(first_outputs.len(), 1);
        let first_output = first_outputs[0]
            .get("output")
            .and_then(Value::as_str)
            .expect("resident tool result output must be a string");
        let (pid, counter) = first_output
            .split_once(':')
            .expect("resident tool output must contain pid and counter");
        assert!(!pid.is_empty());
        assert_eq!(counter, "1");
        assert!(
            !first_events
                .iter()
                .any(|envelope| matches!(&envelope.event, Event::ToolError { .. }))
        );

        engine
            .mail_send(
                lead,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "resident mail two".to_string(),
            )
            .await
            .expect("send second resident mail");
        loop {
            let envelope = bus.recv().await.expect("resident bus remains open");
            if matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    handle: event_handle,
                    status: RosterStatus::Idle,
                    ..
                } if event_handle == &handle
            ) {
                break;
            }
        }
        let second_projection = engine
            .read_projection(lead)
            .await
            .expect("read second resident roster");
        assert_eq!(
            second_projection
                .team
                .roster
                .get(&handle)
                .expect("resident roster entry")
                .status,
            RosterStatus::Idle
        );
        let second_events = engine
            .store()
            .replay(child)
            .await
            .expect("replay second resident child");
        let second_outputs = second_events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::ToolResult { output, .. } => Some(output.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(second_outputs.len(), 2);
        let second_output = second_outputs[1]
            .get("output")
            .and_then(Value::as_str)
            .expect("resident tool result output must be a string");
        let (second_pid, second_counter) = second_output
            .split_once(':')
            .expect("resident tool output must contain pid and counter");
        assert_eq!(second_pid, pid);
        assert_eq!(second_counter, "2");
        assert!(
            !second_events
                .iter()
                .any(|envelope| matches!(&envelope.event, Event::ToolError { .. }))
        );

        supervisor
            .team_cancel(lead)
            .expect("resident team cancel token")
            .cancel();
        std::fs::remove_dir_all(staging_root).expect("cleanup resident staging root");
        std::fs::remove_dir_all(workdir).expect("cleanup resident workdir");
    }

    #[tokio::test]
    async fn resident_idle_sidecar_loss_restarts_before_next_mail() {
        let canonical = "bundle:hya/materialized/tool/echo";
        let mut bundle = materialized_tool_bundle("bun-resident-loss");
        bundle.agent.spawn_lifecycle = SpawnLifecycle::Resident;
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[bundle]).expect("resident loss fixture catalog"),
        ));
        let provider = FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "echo".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("resident loss first complete".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![
                FakeStep::ToolCall {
                    name: "echo".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("resident loss second complete".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ]);
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
            Action::Tool,
            canonical,
            Mode::Allow,
        )]));
        let engine = Arc::new(SessionEngine::new(
            SessionStore::connect_memory()
                .await
                .expect("connect resident loss store"),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog)),
            permission,
            EventBus::default(),
        ));
        let env_root = tempdir();
        let workdir = env_root.join("agent-workdir");
        let env_guard = EnvGuard::set(&env_root, &workdir);
        let lead = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .expect("create resident loss lead");
        let binding = engine
            .bind_runtime(&workdir)
            .expect("capture resident loss binding");
        let old_generation = binding.generation();
        let staging_root = tempdir();
        let socket_path = staging_root.join("sidecar-loss.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket_path)
            .expect("bind sidecar loss listener");
        let socket_literal = serde_json::to_string(socket_path.to_string_lossy().as_ref())
            .expect("encode sidecar loss socket path");
        let fixture = r#"
import json, os, socket, sys
socket_path = __SOCKET__
first_call = True
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "compat"},
            "hooks": [],
            "tools": [{"name": "echo", "description": "sidecar echo", "inputSchema": {"type": "object"}}],
            "workspaceAdapters": []
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif method == "tool/call" and first_call:
        first_call = False
        result = {
            "ok": True,
            "output": {"title": "", "output": str(os.getpid()), "metadata": {}},
            "time_ms": 0
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(socket_path)
        sock.sendall(b"x")
        sock.recv(1)
        sys.stdout.close()
        sock.close()
        break
"#
        .replace("__SOCKET__", &socket_literal);
        let terminate_notify = Arc::new(tokio::sync::Notify::new());
        let mut environment = BundleSidecarEnvironment::from_command(
            vec!["python3".to_string(), "-c".to_string(), fixture],
            staging_root.clone(),
        );
        environment.terminate_notify = Some(Arc::clone(&terminate_notify));
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let allowed = [AgentDef {
            name: "worker".to_string(),
            description: None,
            category: None,
            mode: "subagent".to_string(),
        }];
        let categories = CategoryRegistry::default();
        let is_servable = |_: &ModelRef| true;
        let resolved = resolve_spawn_member(
            &ResolveSpawnMemberCtx {
                engine: &engine,
                binding: &binding,
                base: &base,
                caller: "build",
                allowed_agents: &allowed,
                categories: &categories,
                is_servable: &is_servable,
                guidance: None,
                sidecar_environment: &environment,
            },
            SpawnMember {
                subagent_type: "worker".to_string(),
                resident: true,
                ..SpawnMember::default()
            },
        )
        .expect("resolve resident loss member");
        let ResolvedSpawnMember {
            agent,
            binding,
            agents,
            resources,
            sidecar_factory,
            ..
        } = resolved;
        let supervisor = ResidentSupervisor::start(engine.clone());
        let (child, handle) = supervisor
            .spawn_resident(
                lead,
                agent,
                (
                    binding,
                    agents,
                    resources,
                    Some(sidecar_factory.expect("resident loss sidecar factory")),
                ),
                String::new(),
                None,
                None,
            )
            .await
            .expect("spawn resident loss member");
        let mut bus = engine.bus().subscribe();

        engine
            .mail_send(
                lead,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "resident loss mail one".to_string(),
            )
            .await
            .expect("send first resident loss mail");
        loop {
            let envelope = bus.recv().await.expect("resident loss bus remains open");
            if matches!(
                &envelope.event,
                Event::AgentActivityChanged {
                    handle: event_handle,
                    status: RosterStatus::Idle,
                    ..
                } if event_handle == &handle
            ) {
                break;
            }
        }
        let first_events = engine
            .store()
            .replay(child)
            .await
            .expect("replay first resident loss turn");
        assert_eq!(
            first_events
                .iter()
                .filter(|envelope| matches!(&envelope.event, Event::ToolResult { .. }))
                .count(),
            1
        );

        let first_loss_listener = listener.try_clone().expect("clone sidecar loss listener");
        let first_loss = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::task::spawn_blocking(move || {
                use std::io::{Read as _, Write as _};
                let (mut stream, _) = first_loss_listener.accept()?;
                let mut byte = [0_u8; 1];
                stream.read_exact(&mut byte)?;
                stream.write_all(b"y")?;
                Ok::<u8, std::io::Error>(byte[0])
            }),
        )
        .await
        .expect("sidecar loss must be announced promptly")
        .expect("sidecar loss listener task must join")
        .expect("sidecar loss marker must be readable");
        assert_eq!(first_loss, b'x');
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            terminate_notify.notified(),
        )
        .await
        .expect("resident sidecar terminate cleanup must complete");

        let replacement = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_bundle("resident-replacement")])
                .expect("replacement resident catalog"),
        ));
        let replacement_generation = engine
            .runtime_registry()
            .publish_catalog(replacement)
            .expect("publish replacement catalog");
        assert_ne!(replacement_generation, old_generation);
        let fresh_binding = engine
            .runtime_registry()
            .bind_turn(&workdir)
            .expect("bind replacement resident generation");
        assert_eq!(fresh_binding.generation(), replacement_generation);

        engine
            .mail_send(
                lead,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "resident loss mail two".to_string(),
            )
            .await
            .expect("send second resident loss mail");
        let post_mail_current_task =
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                let mut second_mail_sent = false;
                loop {
                    let envelope = bus.recv().await.expect("resident loss bus remains open");
                    if matches!(
                        &envelope.event,
                        Event::MailSent { body, .. } if body == "resident loss mail two"
                    ) {
                        second_mail_sent = true;
                    }
                    if let Event::AgentActivityChanged {
                        handle: event_handle,
                        status,
                        current_task,
                        ..
                    } = &envelope.event
                        && second_mail_sent
                        && event_handle == &handle
                        && matches!(status, RosterStatus::Idle | RosterStatus::Failed)
                    {
                        break current_task.clone();
                    }
                }
            })
            .await
            .expect("replacement resident must reach Idle or Failed after second mail");
        let second_events = engine
            .store()
            .replay(child)
            .await
            .expect("replay second resident loss turn");
        let successful_results = second_events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::ToolResult { output, .. } => Some(output.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            successful_results.len(),
            2,
            "current_task={post_mail_current_task:?}\n{second_events:#?}"
        );
        assert!(
            !second_events
                .iter()
                .any(|envelope| matches!(&envelope.event, Event::ToolError { .. }))
        );
        let pids = successful_results
            .iter()
            .map(|result| {
                result
                    .get("output")
                    .and_then(Value::as_str)
                    .expect("resident loss tool result output must be a string")
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert!(pids.iter().all(|pid| !pid.is_empty()));
        assert_ne!(pids[0], pids[1]);
        let recorded_generations = second_events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::TurnBindingRecorded { generation, .. } => Some(*generation),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(recorded_generations.len() >= 2);
        assert!(
            recorded_generations
                .iter()
                .all(|generation| *generation == old_generation),
            "resident turns must retain their captured binding generation"
        );

        supervisor
            .team_cancel(lead)
            .expect("resident loss team cancel token")
            .cancel();
        drop(env_guard);
        std::fs::remove_dir_all(staging_root).expect("cleanup resident loss staging root");
        std::fs::remove_dir_all(env_root).expect("cleanup resident loss env root");
    }

    #[tokio::test]
    async fn resident_running_sidecar_loss_fences_epoch_and_resumes_queued_mail_once() {
        let canonical = "bundle:hya/materialized/tool/echo";
        let mut bundle = materialized_tool_bundle("bun-resident-running-loss");
        bundle.agent.spawn_lifecycle = SpawnLifecycle::Resident;
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[bundle]).expect("resident running loss fixture catalog"),
        ));
        let provider = FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "echo".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::ToolCall {
                    name: "echo".to_string(),
                    input: json!({}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("resident queued complete".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ]);
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
            Action::Tool,
            canonical,
            Mode::Allow,
        )]));
        let engine = Arc::new(SessionEngine::new(
            SessionStore::connect_memory()
                .await
                .expect("connect resident running loss store"),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            Arc::new(RuntimeRegistry::new(ToolRegistry::builtins(), catalog)),
            permission,
            EventBus::default(),
        ));
        let env_root = tempdir();
        let workdir = env_root.join("agent-workdir");
        let env_guard = EnvGuard::set(&env_root, &workdir);
        let lead = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: workdir.to_string_lossy().into_owned(),
            })
            .await
            .expect("create resident running loss lead");
        let binding = engine
            .bind_runtime(&workdir)
            .expect("capture resident running loss binding");
        let old_generation = binding.generation();
        let staging_root = tempdir();
        let marker_socket = staging_root.join("resident-running-loss-marker.sock");
        let incarnation_claim = staging_root.join("resident-running-loss-incarnation");
        let release_socket = staging_root.join("resident-running-loss-release.sock");
        let marker_listener = std::os::unix::net::UnixListener::bind(&marker_socket)
            .expect("bind resident running loss marker listener");
        let release_listener = std::os::unix::net::UnixListener::bind(&release_socket)
            .expect("bind resident running loss release listener");
        let marker_literal = serde_json::to_string(marker_socket.to_string_lossy().as_ref())
            .expect("encode resident running loss marker socket");
        let claim_literal = serde_json::to_string(incarnation_claim.to_string_lossy().as_ref())
            .expect("encode resident running loss incarnation claim");
        let release_literal = serde_json::to_string(release_socket.to_string_lossy().as_ref())
            .expect("encode resident running loss release socket");
        let fixture = r#"
import json, os, socket, sys
marker_socket = __MARKER_SOCKET__
claim_path = __CLAIM_PATH__
release_socket = __RELEASE_SOCKET__
try:
    fd = os.open(claim_path, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    os.close(fd)
    incarnation = 1
except FileExistsError:
    incarnation = 2
first_call = True
def send_marker(label):
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(marker_socket)
    sock.sendall((label + ":" + str(os.getpid()) + "\n").encode())
    sock.close()
for line in sys.stdin:
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        result = {
            "protocol_version": 1,
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "compat"},
            "hooks": [],
            "tools": [{"name": "echo", "description": "sidecar echo", "inputSchema": {"type": "object"}}],
            "workspaceAdapters": []
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
        if incarnation == 2:
            send_marker("second")
    elif method == "tool/call":
        if incarnation == 1 and first_call:
            first_call = False
            send_marker("first")
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.connect(release_socket)
            sock.recv(1)
            sock.close()
            sys.stdout.close()
            break
        result = {
            "ok": True,
            "output": {"title": "", "output": str(os.getpid()), "metadata": {}},
            "time_ms": 0
        }
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": result}), flush=True)
    elif method == "shutdown":
        print(json.dumps({"jsonrpc": "2.0", "id": req["id"], "result": True}), flush=True)
        break
"#
        .replace("__MARKER_SOCKET__", &marker_literal)
        .replace("__CLAIM_PATH__", &claim_literal)
        .replace("__RELEASE_SOCKET__", &release_literal);
        let environment = BundleSidecarEnvironment::from_command(
            vec!["python3".to_string(), "-c".to_string(), fixture],
            staging_root.clone(),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let allowed = [AgentDef {
            name: "worker".to_string(),
            description: None,
            category: None,
            mode: "subagent".to_string(),
        }];
        let categories = CategoryRegistry::default();
        let is_servable = |_: &ModelRef| true;
        let resolved = resolve_spawn_member(
            &ResolveSpawnMemberCtx {
                engine: &engine,
                binding: &binding,
                base: &base,
                caller: "build",
                allowed_agents: &allowed,
                categories: &categories,
                is_servable: &is_servable,
                guidance: None,
                sidecar_environment: &environment,
            },
            SpawnMember {
                subagent_type: "worker".to_string(),
                resident: true,
                ..SpawnMember::default()
            },
        )
        .expect("resolve resident running loss member");
        let ResolvedSpawnMember {
            agent,
            binding,
            agents,
            resources,
            sidecar_factory,
            ..
        } = resolved;
        let supervisor = ResidentSupervisor::start(engine.clone());
        let (child, handle) = supervisor
            .spawn_resident(
                lead,
                agent,
                (
                    binding,
                    agents,
                    resources,
                    Some(sidecar_factory.expect("resident running loss sidecar factory")),
                ),
                String::new(),
                None,
                None,
            )
            .await
            .expect("spawn resident running loss member");
        let mut bus = engine.bus().subscribe();
        let first_marker_listener = marker_listener
            .try_clone()
            .expect("clone resident running loss marker listener");
        let first_marker_task = tokio::task::spawn_blocking(move || {
            use std::io::BufRead as _;
            let (stream, _) = first_marker_listener.accept()?;
            let mut line = String::new();
            std::io::BufReader::new(stream).read_line(&mut line)?;
            Ok::<String, std::io::Error>(line)
        });
        let release_task = tokio::task::spawn_blocking(move || {
            let (stream, _) = release_listener.accept()?;
            Ok::<std::os::unix::net::UnixStream, std::io::Error>(stream)
        });

        engine
            .mail_send(
                lead,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "resident running mail one".to_string(),
            )
            .await
            .expect("send first resident running mail");
        let first_marker =
            tokio::time::timeout(std::time::Duration::from_secs(5), first_marker_task)
                .await
                .expect("first resident running marker must arrive")
                .expect("first resident running marker task must join")
                .expect("first resident running marker must be readable");
        let (first_label, first_pid) = first_marker
            .trim()
            .split_once(':')
            .expect("first resident running marker must contain label and pid");
        assert_eq!(first_label, "first");
        let first_pid = first_pid.to_string();

        engine
            .mail_send(
                lead,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "resident running mail two".to_string(),
            )
            .await
            .expect("send queued resident running mail");
        let replacement = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_bundle("resident-running-replacement")])
                .expect("replacement resident running catalog"),
        ));
        let replacement_generation = engine
            .runtime_registry()
            .publish_catalog(replacement)
            .expect("publish replacement resident running catalog");
        assert_ne!(replacement_generation, old_generation);
        let fresh_binding = engine
            .runtime_registry()
            .bind_turn(&workdir)
            .expect("bind replacement resident running generation");
        assert_eq!(fresh_binding.generation(), replacement_generation);

        let second_marker_task = tokio::task::spawn_blocking(move || {
            use std::io::BufRead as _;
            let (stream, _) = marker_listener.accept()?;
            let mut line = String::new();
            std::io::BufReader::new(stream).read_line(&mut line)?;
            Ok::<String, std::io::Error>(line)
        });
        let mut release_stream =
            tokio::time::timeout(std::time::Duration::from_secs(5), release_task)
                .await
                .expect("first resident sidecar release connection must arrive")
                .expect("resident sidecar release task must join")
                .expect("resident sidecar release connection must be accepted");
        use std::io::Write as _;
        release_stream
            .write_all(b"x")
            .expect("release first resident sidecar exactly once");

        let second_marker =
            tokio::time::timeout(std::time::Duration::from_secs(5), second_marker_task)
                .await
                .expect("second resident running marker must arrive")
                .expect("second resident running marker task must join")
                .expect("second resident running marker must be readable");
        let (second_label, second_pid) = second_marker
            .trim()
            .split_once(':')
            .expect("second resident running marker must contain label and pid");
        assert_eq!(second_label, "second");
        let second_pid = second_pid.to_string();

        let observed_finishes = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut finishes = Vec::new();
            while finishes.len() < 2 {
                let envelope = bus.recv().await.expect("resident running bus remains open");
                if let Event::MessageFinished {
                    session,
                    role: hya_proto::Role::Assistant,
                    finish,
                    ..
                } = &envelope.event
                    && *session == child
                {
                    finishes.push(*finish);
                }
            }
            finishes
        })
        .await
        .expect("resident running child finishes must arrive");
        assert_eq!(
            observed_finishes,
            vec![FinishReason::Cancelled, FinishReason::Stop],
            "running loss must cancel the old turn before queued mail stops"
        );

        let child_events = engine
            .store()
            .replay(child)
            .await
            .expect("replay resident running child");
        let durable_finishes = child_events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::MessageFinished {
                    session,
                    role: hya_proto::Role::Assistant,
                    finish,
                    ..
                } if *session == child => Some(*finish),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            durable_finishes,
            vec![FinishReason::Cancelled, FinishReason::Stop],
            "durable child finishes must preserve cancellation before queued stop"
        );
        let assistant_messages = child_events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::MessageStarted {
                    session,
                    message,
                    role: hya_proto::Role::Assistant,
                } if *session == child => Some(*message),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(assistant_messages.len() >= 2);
        let first_message = assistant_messages[0];
        let first_step_count = child_events
            .iter()
            .filter(|envelope| {
                matches!(
                    &envelope.event,
                    Event::StepStarted {
                        session,
                        message,
                        ..
                    } if *session == child && *message == first_message
                )
            })
            .count();
        assert_eq!(
            first_step_count, 1,
            "stale running mail must not continue polling the model"
        );

        let tool_results = child_events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::ToolResult { output, .. } => Some(output),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tool_results.len(), 1);
        assert_eq!(
            tool_results[0].get("output").and_then(Value::as_str),
            Some(second_pid.as_str())
        );
        assert_ne!(first_pid, second_pid);

        let tool_errors = child_events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::ToolError {
                    value,
                    message_text,
                    ..
                } => Some((value, message_text)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tool_errors.len(), 1);
        assert_eq!(
            tool_errors[0]
                .0
                .as_ref()
                .and_then(|value| value.get("code"))
                .and_then(Value::as_str),
            Some("STALE_ACTOR_CLAIM")
        );
        assert!(!tool_errors[0].1.to_ascii_lowercase().contains("closed"));

        let root_events = engine
            .store()
            .replay(lead)
            .await
            .expect("replay resident running root");
        let work_started = root_events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::ResidentWorkStarted {
                    actor_session,
                    epoch,
                    inbox_through,
                    ..
                } if *actor_session == child => Some((*epoch, *inbox_through)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(work_started.len(), 2);
        assert!(work_started[0].0 < work_started[1].0);
        assert_eq!(
            work_started
                .iter()
                .map(|(_, inbox_through)| *inbox_through)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        let recorded_generations = child_events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                Event::TurnBindingRecorded { generation, .. } => Some(*generation),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!recorded_generations.is_empty());
        assert!(
            recorded_generations
                .iter()
                .all(|generation| *generation == old_generation),
            "resident queued work must retain its original binding generation"
        );
        assert_eq!(
            root_events
                .iter()
                .filter(|envelope| {
                    matches!(
                        &envelope.event,
                        Event::MailSent { body, .. } if body == "resident running mail two"
                    )
                })
                .count(),
            1,
            "queued mail must be represented exactly once"
        );

        supervisor
            .team_cancel(lead)
            .expect("resident running team cancel token")
            .cancel();
        drop(env_guard);
        std::fs::remove_dir_all(staging_root).expect("cleanup resident running loss staging root");
        std::fs::remove_dir_all(env_root).expect("cleanup resident running loss env root");
    }

    #[tokio::test]
    async fn bundle_sidecar_rejects_task_context_hook_before_ack() {
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_bundle("unsupported-hook")])
                .expect("unsupported hook fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture unsupported hook turn binding");
        let staging_root = tempdir();
        let fixture =
            activation_sidecar_fixture(r#"[{"name": "message.user.before", "posture": "open"}]"#);
        let environment = BundleSidecarEnvironment::from_command(
            vec!["python3".to_string(), "-c".to_string(), fixture],
            staging_root.clone(),
        );
        let factory = environment
            .factory_for(&binding, "worker")
            .expect("resolve unsupported hook sidecar factory")
            .expect("materialized Bundle must expose its sidecar factory");
        let activation_id = "activation-unsupported-hook";
        let outcome = factory
            .start(SidecarStart {
                activation_id: activation_id.to_string(),
                lifecycle: SidecarLifecycle::Transient,
            })
            .await;
        let detail = match outcome {
            Ok(handle) => {
                drop(handle);
                None
            }
            Err(CoreError::Invalid(detail)) => Some(detail),
            Err(error) => Some(format!("unexpected error: {error}")),
        };
        let activation_removed = !staging_root.join(activation_id).exists();
        std::fs::remove_dir_all(&staging_root).expect("cleanup unsupported hook staging root");
        std::fs::remove_dir_all(&turn_dir).expect("cleanup unsupported hook turn directory");

        let detail = detail.expect("unsupported Bundle sidecar hook declaration must fail");
        assert!(
            detail.contains("unsupported Bundle sidecar hook declaration `message.user.before`"),
            "unexpected unsupported hook error: {detail}"
        );
        assert!(
            activation_removed,
            "unsupported hook rejection must remove its activation directory"
        );
    }

    #[test]
    fn bundle_sidecar_duplicate_hook_declaration_is_rejected() {
        let registrations = [
            HookRegistration {
                name: HookName::Event,
                posture: None,
            },
            HookRegistration {
                name: HookName::Event,
                posture: None,
            },
        ];
        let result = declared_bundle_sidecar_hooks(&registrations);
        let detail = match result {
            Err(CoreError::Invalid(detail)) => detail,
            Ok(_) => "validator returned Ok".to_string(),
            Err(error) => format!("unexpected error: {error}"),
        };
        assert!(
            detail.contains("duplicate Bundle sidecar hook declaration `event`"),
            "duplicate Event declaration must be rejected: {detail}"
        );
    }

    fn materialized_resource(
        marker: &str,
        kind: &str,
        local_id: &str,
        source_path: &str,
    ) -> PreparedResource {
        PreparedResource {
            local_id: local_id.to_string(),
            stable_id: format!("bundle:hya/materialized/{kind}/{local_id}"),
            source_path: source_path.to_string(),
            digest: format!("{marker}-{source_path}-digest"),
            content: format!("{marker} {source_path}\n"),
            aliases: Vec::new(),
        }
    }

    fn materialized_bundle(marker: &str) -> PreparedBundle {
        let mut resource_view = ResourceView::default();
        resource_view.allow.push("echo".to_string());
        PreparedBundle {
            format_version: 1,
            identity: BundleIdentity {
                id: "hya/materialized".to_string(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            digest: format!("{marker}-bundle-digest"),
            agent: PreparedAgent {
                id: AgentName::new("worker"),
                description: None,
                role: AgentRole::Subagent,
                color: None,
                prompt: Some(format!("{marker} worker prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                resource_view,
                can_spawn: Vec::new(),
                hook_refs: vec!["bundle:hya/materialized/hook/event".to_string()],
            },
            tools: vec![materialized_resource(
                marker,
                "tool",
                "echo",
                "extensions/runtime.js",
            )],
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: vec![materialized_resource(
                marker,
                "hook",
                "event",
                "extensions/runtime.js",
            )],
            extensions: vec![materialized_resource(
                marker,
                "extension",
                "runtime",
                "extensions/runtime.js",
            )],
        }
    }

    fn materialized_tool_bundle(marker: &str) -> PreparedBundle {
        let mut bundle = materialized_bundle(marker);
        bundle.hooks.clear();
        bundle.agent.hook_refs.clear();
        bundle
    }

    /// Two bundles with disjoint closures: `alpha` and `beta`, each owning its
    /// own tool, hook, and extension.
    fn disjoint_materialized_bundles(marker: &str) -> Vec<PreparedBundle> {
        let alpha = disjoint_materialized_bundle(marker);
        let mut beta = alpha.clone();
        beta.identity.id = "hya/materialized-beta".to_string();
        beta.digest = format!("{marker}-beta-bundle-digest");
        let beta_tool = format!("bundle:{}/tool/beta", beta.identity.id);
        let beta_hook = format!("bundle:{}/hook/tool.execute.before", beta.identity.id);
        beta.tools = vec![PreparedResource {
            stable_id: beta_tool.clone(),
            ..alpha.tools[1].clone()
        }];
        beta.hooks = vec![PreparedResource {
            stable_id: beta_hook.clone(),
            ..alpha.hooks[1].clone()
        }];
        beta.extensions = vec![PreparedResource {
            stable_id: format!("bundle:{}/extension/beta", beta.identity.id),
            ..alpha.extensions[1].clone()
        }];
        beta.agent = PreparedAgent {
            id: AgentName::new("beta"),
            prompt: Some(format!("{marker} beta prompt")),
            resource_view: ResourceView {
                allow: vec![beta_tool],
                ..ResourceView::default()
            },
            hook_refs: vec![beta_hook],
            ..alpha.agent.clone()
        };

        let mut alpha_only = alpha;
        alpha_only.tools.truncate(1);
        alpha_only.hooks.truncate(1);
        alpha_only.extensions.truncate(1);
        vec![alpha_only, beta]
    }

    fn disjoint_materialized_bundle(marker: &str) -> PreparedBundle {
        let alpha_path = "extensions/alpha.js";
        let beta_path = "extensions/beta.js";
        let alpha_tool = materialized_resource(marker, "tool", "echo", alpha_path);
        let alpha_hook = materialized_resource(marker, "hook", "event", alpha_path);
        let alpha_extension = materialized_resource(marker, "extension", "alpha", alpha_path);
        let beta_tool = materialized_resource(marker, "tool", "beta", beta_path);
        let beta_hook = materialized_resource(marker, "hook", "tool.execute.before", beta_path);
        let beta_extension = materialized_resource(marker, "extension", "beta", beta_path);
        PreparedBundle {
            format_version: 1,
            identity: BundleIdentity {
                id: "hya/materialized".to_string(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            digest: format!("{marker}-disjoint-bundle-digest"),
            // One agent per bundle: `alpha` selects only its own closure, while
            // the bundle still ships both extensions so selection can be
            // exercised over a superset.
            agent: PreparedAgent {
                id: AgentName::new("alpha"),
                description: None,
                role: AgentRole::Subagent,
                color: None,
                prompt: Some(format!("{marker} alpha prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                resource_view: ResourceView {
                    allow: vec![alpha_tool.stable_id.clone()],
                    ..ResourceView::default()
                },
                can_spawn: Vec::new(),
                hook_refs: vec![alpha_hook.stable_id.clone()],
            },
            tools: vec![alpha_tool, beta_tool],
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: vec![alpha_hook, beta_hook],
            extensions: vec![alpha_extension, beta_extension],
        }
    }

    /// Two bundles where one tries to select the other's tool and hook.
    fn cross_bundle_selector_catalog(marker: &str) -> Arc<AgentCatalog> {
        let owner = materialized_bundle(marker);
        let mut selector = owner.clone();
        selector.identity.id = "hya/selector".to_string();
        selector.digest = "selector-bundle-digest".to_string();
        let mut selector_agent = selector.agent.clone();
        selector_agent.id = AgentName::new("selector");
        selector_agent.resource_view = ResourceView {
            allow: vec!["bundle:hya/materialized/tool/echo".to_string()],
            ..ResourceView::default()
        };
        selector_agent.hook_refs = vec!["bundle:hya/materialized/hook/event".to_string()];
        selector.agent = selector_agent;
        selector.tools.clear();
        selector.skills.clear();
        selector.mcp.clear();
        selector.hooks.clear();
        selector.extensions.clear();

        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[selector, owner])
                .expect("cross-bundle materialization fixture catalog"),
        ))
    }

    #[test]
    fn a_bundle_cannot_select_another_bundles_tool() {
        // A bundle agent's plane admits only its OWN bundle resources. Before
        // this rule, the allow-driven re-resolution searched every bundle, so a
        // selector could borrow the owner's tool.
        let catalog = cross_bundle_selector_catalog("owner");
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture cross-bundle turn binding");

        let refused = binding.has_selected_bundle_sidecar_capability("selector");
        std::fs::remove_dir_all(&turn_dir).expect("cleanup turn directory");

        let Err(hya_bundle::BundleError::ResourceNotInPlane { reference, .. }) = refused else {
            panic!("cross-bundle selection must be refused, got {refused:?}");
        };
        assert_eq!(reference, "bundle:hya/materialized/tool/echo");
    }

    fn set_materialized_extension_content(bundle: &mut PreparedBundle, content: String) {
        let digest = format!(
            "materialized-content-{:x}",
            Sha256::digest(content.as_bytes())
        );
        for resource in bundle
            .tools
            .iter_mut()
            .chain(bundle.hooks.iter_mut())
            .chain(bundle.extensions.iter_mut())
        {
            resource.content = content.clone();
            resource.digest = digest.clone();
        }
    }

    fn set_materialized_extension_content_for_path(
        bundle: &mut PreparedBundle,
        source_path: &str,
        content: String,
    ) {
        let digest = format!(
            "materialized-content-{:x}",
            Sha256::digest(content.as_bytes())
        );
        for resource in bundle
            .tools
            .iter_mut()
            .chain(bundle.hooks.iter_mut())
            .chain(bundle.extensions.iter_mut())
        {
            if resource.source_path == source_path {
                resource.content = content.clone();
                resource.digest = digest.clone();
            }
        }
    }

    fn materialized_bun_bundle(marker: &str) -> PreparedBundle {
        let mut bundle = materialized_tool_bundle(marker);
        set_materialized_extension_content(
            &mut bundle,
            r#"
export default {
  id: "bundle-extension",
  server: async (input) => {
    if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
      throw new Error("bundle extension received unexpected initialization input")
    }
    return {
      tool: {
        echo: {
          description: "bundle echo",
          execute: async (input) => {
            if (input === null || typeof input !== "object" || Object.keys(input).length !== 0) {
              throw new Error("bundle echo received unexpected tool input")
            }
            return "bundle-e2e"
          },
        },
      },
    }
  },
}
"#
            .to_string(),
        );
        bundle
    }

    #[test]
    fn sidecar_resources_materialize_from_captured_turn_binding() {
        let old_catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_bundle("old")])
                .expect("old fixture catalog"),
        ));
        let new_catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_bundle("new")])
                .expect("new fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), Arc::clone(&old_catalog));
        let turn_dir = tempdir();
        let old_binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture old turn binding");
        runtime
            .publish_catalog(new_catalog)
            .expect("publish replacement catalog");

        let activation_root = tempdir();
        let activation_dir = activation_root.join("activation");
        std::fs::create_dir_all(&activation_dir).expect("create activation directory");
        let materialized =
            materialize_bundle_sidecar_resources(&old_binding, "worker", &activation_dir)
                .expect("materialize captured bundle resources");

        assert_eq!(
            materialized,
            vec![activation_dir.join("extensions/runtime.js")]
        );
        assert_eq!(
            std::fs::read_to_string(activation_dir.join("extensions/runtime.js"))
                .expect("read materialized extension"),
            "old extensions/runtime.js\n"
        );
        let materialized_files = ["extensions/runtime.js"];
        for relative_path in materialized_files {
            let content = std::fs::read_to_string(activation_dir.join(relative_path))
                .expect("read materialized resource");
            assert!(
                !content.contains("new"),
                "captured resources must not use new catalog"
            );
        }

        std::fs::remove_dir_all(activation_root).expect("cleanup activation directory");
        std::fs::remove_dir_all(turn_dir).expect("cleanup turn directory");
    }

    #[test]
    fn sidecar_materialization_uses_only_captured_agent_selected_closure() {
        let old_catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&disjoint_materialized_bundles("old"))
                .expect("old disjoint fixture catalog"),
        ));
        let new_catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&disjoint_materialized_bundles("new"))
                .expect("new disjoint fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), Arc::clone(&old_catalog));
        let old_turn_dir = tempdir();
        let old_binding = runtime
            .bind_turn(&old_turn_dir)
            .expect("capture old disjoint turn binding");
        runtime
            .publish_catalog(new_catalog)
            .expect("publish disjoint replacement catalog");

        let alpha_root = tempdir();
        let alpha_dir = alpha_root.join("activation");
        std::fs::create_dir_all(&alpha_dir).expect("create alpha activation directory");
        let alpha_materialized =
            materialize_bundle_sidecar_resources(&old_binding, "alpha", &alpha_dir)
                .expect("materialize alpha captured resources");
        assert_eq!(
            alpha_materialized,
            vec![alpha_dir.join("extensions/alpha.js")]
        );
        assert_eq!(
            std::fs::read_to_string(alpha_dir.join("extensions/alpha.js"))
                .expect("read alpha materialized extension"),
            "old extensions/alpha.js\n"
        );
        assert!(!alpha_dir.join("extensions/beta.js").exists());

        let beta_root = tempdir();
        let beta_dir = beta_root.join("activation");
        std::fs::create_dir_all(&beta_dir).expect("create beta activation directory");
        let beta_materialized =
            materialize_bundle_sidecar_resources(&old_binding, "beta", &beta_dir)
                .expect("materialize beta captured resources");
        assert_eq!(beta_materialized, vec![beta_dir.join("extensions/beta.js")]);
        assert_eq!(
            std::fs::read_to_string(beta_dir.join("extensions/beta.js"))
                .expect("read beta materialized extension"),
            "old extensions/beta.js\n"
        );
        assert!(!beta_dir.join("extensions/alpha.js").exists());

        let new_turn_dir = tempdir();
        let new_binding = runtime
            .bind_turn(&new_turn_dir)
            .expect("capture new disjoint turn binding");
        let new_root = tempdir();
        let new_dir = new_root.join("activation");
        std::fs::create_dir_all(&new_dir).expect("create new activation directory");
        let new_materialized =
            materialize_bundle_sidecar_resources(&new_binding, "alpha", &new_dir)
                .expect("materialize new captured alpha resources");
        assert_eq!(new_materialized, vec![new_dir.join("extensions/alpha.js")]);
        assert_eq!(
            std::fs::read_to_string(new_dir.join("extensions/alpha.js"))
                .expect("read new alpha materialized extension"),
            "new extensions/alpha.js\n"
        );
        assert!(!new_dir.join("extensions/beta.js").exists());

        std::fs::remove_dir_all(alpha_root).expect("cleanup alpha activation directory");
        std::fs::remove_dir_all(beta_root).expect("cleanup beta activation directory");
        std::fs::remove_dir_all(new_root).expect("cleanup new activation directory");
        std::fs::remove_dir_all(old_turn_dir).expect("cleanup old turn directory");
        std::fs::remove_dir_all(new_turn_dir).expect("cleanup new turn directory");
    }

    #[test]
    fn sidecar_materialization_orders_deduplicated_extensions_by_canonical_identity() {
        let mut bundle = disjoint_materialized_bundle("ordered");
        bundle
            .agent
            .resource_view
            .allow
            .push(bundle.tools[1].stable_id.clone());
        bundle.extensions[0].local_id = "zeta".to_string();
        bundle.extensions[0].stable_id = "bundle:hya/materialized/extension/zeta".to_string();
        bundle.extensions[1].local_id = "alpha".to_string();
        bundle.extensions[1].stable_id = "bundle:hya/materialized/extension/alpha".to_string();

        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[bundle])
                .expect("ordered materialization fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture ordered materialization turn binding");
        let activation_root = tempdir();
        let activation_dir = activation_root.join("activation");
        std::fs::create_dir_all(&activation_dir).expect("create activation directory");

        let materialized = materialize_bundle_sidecar_resources(&binding, "alpha", &activation_dir)
            .expect("materialize ordered captured resources");
        let alpha_content = std::fs::read_to_string(activation_dir.join("extensions/alpha.js"))
            .expect("read alpha extension");
        let beta_content = std::fs::read_to_string(activation_dir.join("extensions/beta.js"))
            .expect("read beta extension");

        std::fs::remove_dir_all(activation_root).expect("cleanup activation directory");
        std::fs::remove_dir_all(turn_dir).expect("cleanup turn directory");

        assert_eq!(
            materialized,
            vec![
                activation_dir.join("extensions/beta.js"),
                activation_dir.join("extensions/alpha.js"),
            ]
        );
        assert_eq!(alpha_content, "ordered extensions/alpha.js\n");
        assert_eq!(beta_content, "ordered extensions/beta.js\n");
    }

    #[tokio::test]
    async fn bundle_sidecar_tool_declaration_binds_canonical_captured_resource() {
        let old_catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_bundle("old")])
                .expect("old fixture catalog"),
        ));
        let mut new_bundle = materialized_bundle("new");
        new_bundle.tools.clear();
        let new_catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[new_bundle]).expect("new fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), Arc::clone(&old_catalog));
        let turn_dir = tempdir();
        let old_binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture old turn binding");
        runtime
            .publish_catalog(new_catalog)
            .expect("publish replacement catalog");

        let client = PluginClient::new(tokio::io::empty(), tokio::io::sink());
        let declaration = ToolInfo {
            name: "echo".to_string(),
            description: "sidecar echo".to_string(),
            input_schema: json!({"type": "object"}),
        };
        let bindings =
            super::bind_bundle_sidecar_tools(&old_binding, "worker", &client, &[declaration])
                .expect("bind sidecar tool declaration");

        assert_eq!(bindings.len(), 1);
        let resolved = &bindings[0];
        assert_eq!(resolved.tool.name(), "bundle:hya/materialized/tool/echo");
        let schema = resolved.tool.schema();
        assert_eq!(schema.name.as_str(), "bundle:hya/materialized/tool/echo");
        assert_eq!(schema.description, "sidecar echo");
        assert_eq!(schema.input_schema, json!({"type": "object"}));
        assert_eq!(resolved.permission, ToolPermission::Tool);
    }

    #[tokio::test]
    async fn bundle_sidecar_tool_declarations_match_captured_selected_set() {
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[disjoint_materialized_bundle("selected-tools")])
                .expect("selected-tools fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture selected-tools turn binding");
        let client = PluginClient::new(tokio::io::empty(), tokio::io::sink());
        let echo = ToolInfo {
            name: "echo".to_string(),
            description: "selected echo".to_string(),
            input_schema: json!({"type": "object"}),
        };
        assert!(matches!(
            super::bind_bundle_sidecar_tools(&binding, "alpha", &client, &[]),
            Err(CoreError::Invalid(_))
        ));
        let bindings = super::bind_bundle_sidecar_tools(&binding, "alpha", &client, &[echo])
            .expect("selected tool declaration must bind");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].tool.name(), "bundle:hya/materialized/tool/echo");

        let declarations = [
            ToolInfo {
                name: "echo".to_string(),
                description: "selected echo".to_string(),
                input_schema: json!({"type": "object"}),
            },
            ToolInfo {
                name: "beta".to_string(),
                description: "unselected beta".to_string(),
                input_schema: json!({"type": "object"}),
            },
        ];
        assert!(matches!(
            super::bind_bundle_sidecar_tools(&binding, "alpha", &client, &declarations),
            Err(CoreError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn bundle_sidecar_initialize_declarations_ignore_tool_and_hook_order() {
        let mut bundle = disjoint_materialized_bundle("order-independent");
        let selected_tool_ids = bundle
            .tools
            .iter()
            .map(|resource| resource.stable_id.clone())
            .collect::<Vec<_>>();
        let selected_hook_ids = bundle
            .hooks
            .iter()
            .map(|resource| resource.stable_id.clone())
            .collect::<Vec<_>>();
        bundle.agent.resource_view.allow = selected_tool_ids;
        bundle.agent.hook_refs = selected_hook_ids;

        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[bundle])
                .expect("order-independent declaration fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture order-independent declaration turn binding");

        let declarations = [
            ToolInfo {
                name: "echo".to_string(),
                description: "selected echo".to_string(),
                input_schema: json!({"type": "object"}),
            },
            ToolInfo {
                name: "beta".to_string(),
                description: "selected beta".to_string(),
                input_schema: json!({"type": "object"}),
            },
        ];
        let registrations = [
            HookRegistration {
                name: HookName::ToolExecuteBefore,
                posture: None,
            },
            HookRegistration {
                name: HookName::Event,
                posture: None,
            },
        ];

        assert!(validate_bundle_sidecar_hooks(&binding, "alpha", &registrations).is_ok());

        let client = PluginClient::new(tokio::io::empty(), tokio::io::sink());
        let bindings = bind_bundle_sidecar_tools(&binding, "alpha", &client, &declarations)
            .expect("reverse-order tool declarations must bind");
        let canonical_names = bindings
            .iter()
            .map(|resolved| resolved.tool.name().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            canonical_names,
            vec![
                "bundle:hya/materialized/tool/beta".to_string(),
                "bundle:hya/materialized/tool/echo".to_string(),
            ]
        );

        std::fs::remove_dir_all(turn_dir).expect("cleanup order-independent turn directory");
    }

    #[tokio::test]
    async fn bundle_sidecar_hook_only_declaration_accepts_zero_tools() {
        let mut bundle = materialized_bundle("hook-only");
        bundle.tools.clear();
        bundle.agent.resource_view = ResourceView::default();
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[bundle]).expect("hook-only fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture hook-only turn binding");
        let registrations = [HookRegistration {
            name: HookName::Event,
            posture: None,
        }];
        assert!(super::validate_bundle_sidecar_hooks(&binding, "worker", &registrations).is_ok());

        let client = PluginClient::new(tokio::io::empty(), tokio::io::sink());
        let bindings = super::bind_bundle_sidecar_tools(&binding, "worker", &client, &[])
            .expect("hook-only activation accepts zero tool declarations");
        assert!(bindings.is_empty());

        std::fs::remove_dir_all(turn_dir).expect("cleanup hook-only turn directory");
    }

    #[tokio::test]
    async fn bundle_sidecar_tool_cancellation_returns_without_rpc_reply() {
        let session = SessionId::new();
        let call = hya_proto::ToolCallId::new();
        let canonical_name = "bundle:hya/materialized/tool/echo".to_string();
        let input = json!({"text": "cancel"});
        let expected_input = input.clone();
        let (client_io, server_io) = tokio::io::duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_io);
        let client = hya_plugin::PluginClient::new(client_read, client_write);
        let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(server_io).lines();
            let line = lines
                .next_line()
                .await
                .expect("read Bundle sidecar tool request")
                .expect("Bundle sidecar tool request must be present");
            let request = match Frame::parse(&line).expect("parse Bundle sidecar request") {
                Frame::Request(request) => request,
                frame => panic!("Bundle sidecar tool call must be a request: {frame:?}"),
            };
            assert_eq!(request.method, METHOD_TOOL_CALL);
            let params: ToolCallParams =
                serde_json::from_value(request.params).expect("decode Bundle sidecar params");
            assert_eq!(params.tool, "echo");
            assert_eq!(params.session, session);
            assert_eq!(params.call, call);
            assert_eq!(params.input, expected_input);
            let _ = observed_tx.send(());
            std::future::pending::<()>().await;
        });

        let (permission, _permission_rx) =
            PermissionPlane::new(PermissionRules::new(vec![Rule::new(
                Action::Tool,
                canonical_name.clone(),
                Mode::Allow,
            )]));
        let (interaction, _interaction_rx) = InteractionPlane::new();
        let (spawner, _spawner_rx) = SpawnerPlane::new();
        let ctx = ToolCtx {
            workflows: hya_tool::WorkflowPlane::disconnected(),
            permission: permission.for_session(session),
            interaction: interaction.for_session(session),
            spawner,
            operation: ToolOperation::from_tool_call(call),
            mailbox: MailboxPlane::disconnected(),
            session: Some(session),
            parent_session: None,
            todo: TodoPlane::default(),
            skills: SkillPlane::default(),
            websearch: WebSearchPlane::default(),
            formatter: FormatterPlane::default(),
            agents: Default::default(),
            lsp: LspPlane::default(),
            workdir: PathBuf::from("."),
            cancel: Default::default(),
        };
        let cancel = ctx.cancel.clone();
        let tool = BundleSidecarTool {
            client,
            rpc_name: "echo".to_string(),
            canonical_name: canonical_name.clone(),
            schema: ToolSchema {
                name: ToolName::new(canonical_name),
                description: "sidecar echo".to_string(),
                input_schema: json!({"type": "object"}),
                output_schema: None,
            },
        };
        let mut execute = Box::pin(tool.execute(&ctx, input));
        let outcome = tokio::time::timeout(std::time::Duration::from_millis(250), async {
            tokio::select! {
                result = &mut execute => (false, result),
                observed = observed_rx => {
                    if observed.is_ok() {
                        cancel.cancel();
                        (true, execute.await)
                    } else {
                        (false, Err(ToolError::Other("server did not observe Bundle sidecar tool request".to_string())))
                    }
                }
            }
        })
        .await;

        server.abort();
        let _ = server.await;

        let outcome = outcome.expect("Bundle sidecar tool must stop after cancellation");
        assert!(matches!(outcome, (true, Err(ToolError::Cancelled))));
    }

    #[test]
    fn sidecar_factory_is_scoped_to_the_selected_agent_effective_capability() {
        // Two bundles: `worker` selects the bundle tool, `lead` denies it. The
        // sidecar factory follows each agent's own effective capability.
        let mut worker_bundle = materialized_bundle("selected-agent");
        worker_bundle.agent.spawn_lifecycle = SpawnLifecycle::Resident;

        let mut lead_bundle = worker_bundle.clone();
        lead_bundle.identity.id = "hya/selected-agent-lead".to_string();
        lead_bundle.digest = "selected-agent-lead-digest".to_string();
        lead_bundle.tools.clear();
        lead_bundle.hooks.clear();
        lead_bundle.extensions.clear();
        lead_bundle.agent = PreparedAgent {
            id: AgentName::new("lead"),
            description: None,
            role: AgentRole::Main,
            color: None,
            prompt: Some("selected-agent lead prompt".to_string()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            spawn_lifecycle: SpawnLifecycle::Transient,
            resource_view: ResourceView::default(),
            can_spawn: vec![AgentName::new("worker")],
            hook_refs: Vec::new(),
        };

        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[lead_bundle, worker_bundle])
                .expect("selected-agent fixture catalog"),
        ));
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture selected-agent turn binding");
        let staging_root = tempdir();
        let environment = BundleSidecarEnvironment::from_command(
            vec!["bun".to_string(), "sidecar.js".to_string()],
            staging_root.clone(),
        );

        let build_factory = environment
            .factory_for(&binding, "build")
            .expect("resolve build sidecar capability");
        let worker_factory = environment
            .factory_for(&binding, "worker")
            .expect("resolve worker sidecar capability");
        let build_is_static = build_factory.is_none();
        let worker_is_executable = worker_factory.is_some();

        std::fs::remove_dir_all(turn_dir).expect("cleanup selected-agent turn directory");
        std::fs::remove_dir_all(staging_root).expect("cleanup selected-agent staging root");

        assert!(
            build_is_static,
            "build must stay process-free when its effective view denies bundle echo"
        );
        assert!(
            worker_is_executable,
            "worker must retain the executable sidecar capability"
        );
    }

    #[test]
    fn executable_bundle_builds_bound_sidecar_factory_while_static_bundle_stays_process_free() {
        let executable_catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[materialized_bundle("executable")])
                .expect("executable fixture catalog"),
        ));
        let executable_runtime = RuntimeRegistry::new(ToolRegistry::builtins(), executable_catalog);
        let executable_turn_dir = tempdir();
        let executable_binding = executable_runtime
            .bind_turn(&executable_turn_dir)
            .expect("capture executable turn binding");
        let executable_staging = tempdir();
        let executable_environment = BundleSidecarEnvironment::from_command(
            vec!["bun".to_string(), "sidecar.js".to_string()],
            executable_staging.clone(),
        );
        let executable_factory = executable_environment
            .factory_for(&executable_binding, "worker")
            .expect("resolve executable sidecar factory");
        assert!(
            executable_factory.is_some(),
            "bundle extension must expose an activation factory"
        );

        let mut static_bundle = materialized_bundle("static");
        static_bundle.extensions.clear();
        static_bundle.tools.clear();
        static_bundle.hooks.clear();
        static_bundle.agent.hook_refs.clear();
        static_bundle.agent.resource_view = ResourceView::default();
        let static_catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&[static_bundle]).expect("static fixture catalog"),
        ));
        let static_runtime = RuntimeRegistry::new(ToolRegistry::builtins(), static_catalog);
        let static_turn_dir = tempdir();
        let static_binding = static_runtime
            .bind_turn(&static_turn_dir)
            .expect("capture static turn binding");
        let static_staging = tempdir();
        let static_environment = BundleSidecarEnvironment::from_command(
            vec!["bun".to_string(), "sidecar.js".to_string()],
            static_staging.clone(),
        );
        let static_factory = static_environment
            .factory_for(&static_binding, "worker")
            .expect("resolve static sidecar factory");
        assert!(
            static_factory.is_none(),
            "static-only bundle must remain process-free"
        );

        std::fs::remove_dir_all(executable_turn_dir).expect("cleanup executable turn directory");
        std::fs::remove_dir_all(executable_staging).expect("cleanup executable staging");
        std::fs::remove_dir_all(static_turn_dir).expect("cleanup static turn directory");
        std::fs::remove_dir_all(static_staging).expect("cleanup static staging");
    }

    #[test]
    fn category_model_fallbacks_seeds_forward_suffix_chains() {
        let mut entries = HashMap::new();
        entries.insert(
            "deep".to_string(),
            CategoryEntry::from_candidates(&[
                "provider/first".to_string(),
                "provider/second".to_string(),
                "provider/third".to_string(),
            ])
            .expect("non-empty candidates"),
        );
        entries.insert(
            "single".to_string(),
            CategoryEntry::from_candidates(&["provider/only".to_string()])
                .expect("non-empty candidates"),
        );
        let fallbacks = category_model_fallbacks(&CategoryRegistry::from_entries(entries));

        assert_eq!(fallbacks.len(), 2, "mid-chain pick and preference only");
        assert_eq!(
            fallbacks
                .get(&ModelRef::new("provider/first"))
                .map(Vec::as_slice),
            Some(
                ["provider/first", "provider/second", "provider/third"]
                    .map(ModelRef::new)
                    .as_slice()
            )
        );
        assert_eq!(
            fallbacks
                .get(&ModelRef::new("provider/second"))
                .map(Vec::as_slice),
            Some(
                ["provider/second", "provider/third"]
                    .map(ModelRef::new)
                    .as_slice()
            ),
            "servability picks mid-chain keep forward failover"
        );
        assert!(!fallbacks.contains_key(&ModelRef::new("provider/only")));
        assert_eq!(
            category_model_fallbacks(&CategoryRegistry::default()).len(),
            0
        );
    }

    /// Regression contract for user-assembled workflows: a stage member's
    /// nested `task` call is resolved against the STAGE agent's OWN roster —
    /// exactly like the task-tool path resolves any spawn target. A target the
    /// parent may spawn must still be refused inside a stage whose agent has no
    /// matching `can_spawn`, and spawned end-to-end when it does.
    #[tokio::test]
    async fn workflow_stage_nested_task_follows_the_stage_agents_own_roster() {
        let _env = StableEnvGuard::acquire();
        let make_bundle = |stable_id: &str, can_spawn: &[&str]| {
            let can_spawn: Vec<_> = can_spawn.iter().map(|id| AgentName::new(*id)).collect();
            PreparedBundle {
                format_version: 1,
                identity: BundleIdentity {
                    id: format!("hya/workflow-roster-{stable_id}"),
                    version: "0.0.0".to_string(),
                    publisher: "hya-tests".to_string(),
                },
                digest: format!("test-only-{stable_id}"),
                agent: PreparedAgent {
                    id: AgentName::new(stable_id),
                    description: None,
                    role: AgentRole::Subagent,
                    color: None,
                    prompt: Some(format!("{stable_id} prompt")),
                    prompt_source: None,
                    prompt_digest: None,
                    model_policy: ModelPolicy::default(),
                    workdir: None,
                    spawn_lifecycle: SpawnLifecycle::Transient,
                    resource_view: ResourceView::default(),
                    can_spawn,
                    hook_refs: Vec::new(),
                },
                tools: Vec::new(),
                skills: Vec::new(),
                mcp: Vec::new(),
                hooks: Vec::new(),
                extensions: Vec::new(),
            }
        };
        async fn stage_stack(
            scripts: Vec<Vec<FakeStep>>,
            bundles: &[PreparedBundle],
            workdir: &std::path::Path,
        ) -> (
            Arc<SessionEngine>,
            AgentSpec,
            tokio::sync::broadcast::Receiver<hya_proto::Envelope>,
            SpawnSupervisorLifecycle,
        ) {
            let provider = Arc::new(FakeProvider::scripted_turns(scripts));
            let router = Arc::new(ProviderRouter::new().with(provider));
            let catalog = Arc::new(to_agent_catalog(
                BundleCatalog::from_prepared(bundles).expect("valid workflow roster bundles"),
            ));
            let runtime = Arc::new(RuntimeRegistry::from_snapshot(
                ToolRegistry::builtins().snapshot(),
                catalog,
            ));
            let rules = PermissionRules::new(vec![Rule::new(Action::Task, "*", Mode::Allow)]);
            let (permission, _permission_rx) = PermissionPlane::new(rules);
            let bus = EventBus::default();
            let events = bus.subscribe();
            let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(8);
            let engine = Arc::new(
                SessionEngine::new(
                    SessionStore::connect_memory()
                        .await
                        .expect("in-memory store"),
                    router.clone(),
                    runtime,
                    permission,
                    bus,
                )
                .with_spawn_sender(spawn_sender),
            );
            let base = AgentSpec {
                name: AgentName::new("build"),
                model: ModelRef::new("fake"),
                system_prompt: "workflow stage base".to_string(),
                workdir: workdir.to_path_buf(),
                reasoning: None,
            };
            let resident_supervisor = ResidentSupervisor::start(Arc::clone(&engine));
            let supervisor = spawn_team_supervisor_with_environment(
                spawn_rx,
                Arc::clone(&engine),
                base.clone(),
                router,
                Arc::new(CategoryRegistry::default()),
                resident_supervisor,
                Arc::new(BundleSidecarEnvironment {
                    command: None,
                    staging_root: tempdir(),
                    terminate_notify: None,
                    test_observer: None,
                    uniform_probe: None,
                }),
            );
            (engine, base, events, supervisor)
        }
        async fn run_stage_workflow(
            engine: &Arc<SessionEngine>,
            base: &AgentSpec,
            workdir: &std::path::Path,
            stage_agent: &str,
        ) -> hya_core::WorkflowRunReport {
            let source = format!(
                r#"---
kind: Workflow
name: nested-task-flow
description: One Stage whose member attempts a nested task.
nodes:
  worker_stage:
    agent: {stage_agent}
    directive: NESTED_TASK_STAGE
---
flowchart TD
  worker_stage
"#
            );
            let def = hya_workflow::compile(hya_workflow::WorkflowSource::new(
                "nested-task.hya.md",
                &source,
            ))
            .expect("valid Workflow definition");
            let lead = engine
                .create(CreateSession {
                    parent: None,
                    agent: base.name.clone(),
                    model: base.model.clone(),
                    workdir: workdir.to_string_lossy().into_owned(),
                })
                .await
                .expect("lead session");
            let binding = engine.bind_runtime(workdir).expect("bind workflow run");
            let caller_base = engine
                .agent_spec_for_binding(&binding, base, base.name.as_str())
                .expect("base spec resolution");
            let context = hya_core::WorkflowRunContext {
                binding,
                caller: base.name.to_string(),
                base_agent: caller_base,
                inputs: std::collections::BTreeMap::new(),
                resident_supervisor: None,
            };
            hya_core::run_workflow(
                engine.clone(),
                lead,
                &def,
                context,
                CancellationToken::new(),
            )
            .await
            .expect("workflow completes")
        }

        // ---- Phase 1: refusal. `isolated_worker` cannot itself spawn
        // `general`, even though the CALLER (`build`) is authorized to do so.
        {
            let bundles = vec![make_bundle("isolated_worker", &[])];
            let workdir = tempdir();
            let scripts = vec![
                vec![
                    FakeStep::ToolCall {
                        name: "task".to_string(),
                        input: json!({
                            "description": "nested attempt",
                            "prompt": "GRANDCHILD_WORK",
                            "subagent_type": "general"
                        }),
                    },
                    FakeStep::Finish(FinishReason::ToolCalls),
                ],
                vec![
                    FakeStep::Text("worker gave up".to_string()),
                    FakeStep::Finish(FinishReason::Stop),
                ],
            ];
            let (engine, base, mut events, supervisor) =
                stage_stack(scripts, &bundles, &workdir).await;
            let report = run_stage_workflow(&engine, &base, &workdir, "isolated_worker").await;
            drop(supervisor);
            assert_eq!(report.status, hya_core::WorkflowStatus::Completed);
            assert_eq!(report.stages.len(), 1);
            assert_eq!(report.stages[0].status.to_string(), "done");
            let child = report.stages[0]
                .session
                .as_deref()
                .expect("member session")
                .to_string();
            let rejections: Vec<String> = {
                let mut out = Vec::new();
                while let Ok(envelope) = events.try_recv() {
                    if let Event::ToolError {
                        session,
                        message_text,
                        ..
                    } = envelope.event
                    {
                        out.push((session.to_string(), message_text));
                    }
                }
                out.into_iter()
                    .filter(|(session, text)| {
                        *session == child && text.contains("AGENT_SPAWN_NOT_ALLOWED")
                    })
                    .map(|(_, text)| text)
                    .collect()
            };
            assert!(
                rejections.iter().any(|text| text.contains("`general`")),
                "the stage's task call must be refused for its own roster: {rejections:?}"
            );
            // No grandchild session may exist anywhere in the team tree.
            let sessions = engine.store().list_sessions().await.expect("sessions");
            assert_eq!(
                sessions.len(),
                2,
                "only the lead and the stage member exist; grandchild must not spawn"
            );
        }

        // ---- Positive control: the SAME production seam that resolves every
        // member spawn (including workflow children's nested task calls)
        // accepts `general` for the DELEGATING stage roster and refuses it for
        // the ISOLATED stage roster, while the caller keeps both stages.
        {
            let bundles = vec![
                make_bundle("isolated_worker", &[]),
                make_bundle("delegating_worker", &["general"]),
            ];
            let catalog =
                BundleCatalog::from_prepared(&bundles).expect("valid workflow roster bundles");
            let catalog = Arc::new(to_agent_catalog(catalog));
            let runtime = Arc::new(RuntimeRegistry::from_snapshot(
                ToolRegistry::builtins().snapshot(),
                catalog,
            ));
            let router = Arc::new(ProviderRouter::new());
            let store = SessionStore::connect_memory().await.expect("store");
            let engine = Arc::new(SessionEngine::new(
                store,
                router,
                runtime,
                PermissionPlane::new(PermissionRules::default()).0,
                EventBus::default(),
            ));
            let workdir = tempdir();
            let binding = engine.bind_runtime(&workdir).expect("bind");
            let member = |target: &str| SpawnMember {
                description: "nested".to_string(),
                prompt: "GRANDCHILD_WORK".to_string(),
                subagent_type: target.to_string(),
                task_id: None,
                model: None,
                category: None,
                inline_agent: None,
                resident: false,
            };
            let delegating_roster = engine
                .agent_roster_for_binding(&binding, "delegating_worker")
                .expect("delegating roster");
            assert!(
                authorize_spawn_target(
                    &binding,
                    delegating_roster.as_ref(),
                    "delegating_worker",
                    &member("general"),
                )
                .is_ok(),
                "own can_spawn grants delegation"
            );
            let isolated_roster = engine
                .agent_roster_for_binding(&binding, "isolated_worker")
                .expect("isolated roster");
            let refused = authorize_spawn_target(
                &binding,
                isolated_roster.as_ref(),
                "isolated_worker",
                &member("general"),
            )
            .expect_err("empty own roster must refuse");
            assert!(
                matches!(refused, SpawnError::AgentSpawnNotAllowed { ref agent_id, .. } if agent_id == "general"),
                "unexpected refusal: {refused:?}"
            );
            // Caller authority is untouched: both stages remain admittable.
            assert!(binding.resolve_spawn("build", "isolated_worker").is_ok());
            assert!(binding.resolve_spawn("build", "delegating_worker").is_ok());
        }
    }
}
