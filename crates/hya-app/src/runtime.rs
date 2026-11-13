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
use hya_bundle::BundleCatalog;
use hya_core::agent_catalog::{AgentCatalog, AgentDefinition};
use hya_core::{
    AgentResourcePolicy, AgentSpec, BoundSidecarFactory, BoundSpawnRequest, BoundSpawnSender,
    BoundWorkflowRequest, BoundWorkflowSender, CategoryRegistry, CoreError, DRAIN_DEADLINE,
    EventBus, ModelSummarizer, PromptEnv, ResidentSupervisor, RuntimeRegistry, RuntimeSourceKind,
    SessionEngine, SidecarEnvironment, SidecarHandle, SidecarLifecycle, SidecarStart,
    SubagentGovernor, Summarizer, TaskSpawnOrigin, TokenAccounting, TurnBinding, TurnDrainReport,
    apply_agent_model_preference, apply_spawn_model_policy, build_system_prompt,
    resolve_dispatch_model, run_lifecycle_service, run_mailbox_service,
};

// Single discovery/date implementation lives in hya-core; re-export for callers.
pub use hya_core::{discover_context_files, today};
use hya_mcp::McpServerConfig;
use hya_plugin::config::{PluginSpec, ProcessLaunch};
use hya_plugin::messages::{
    ActivationLifecycle, ActivationMetadata, HookName, HookRegistration, PluginKindWire,
};
use hya_plugin::{HostInfo, PluginContributionSet, PluginHost, PreparedPlugin};
use hya_proto::{
    AgentName, FinishCause, ModelRef, OwnerRunId, SessionId, SubagentMode, WorkflowDelivery,
};
use hya_provider::{DevProvider, ProviderCatalogSnapshot, ProviderRouter, ReasoningEffort};
use hya_store::{SessionStore, StoreError};
use hya_tool::{
    Action, AskRequest, InteractionPlane, InvocationPolicy, LifecyclePlane, MailboxPlane,
    MemberOutcome, Mode, PermissionModel, PermissionPlane, PermissionRules, QuestionRequest,
    ResolvedTool, Rule, SpawnError, SpawnMember, ToolPermission, ToolRegistry, WebSearchConfig,
    WebSearchPlane,
};
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::OpenOptions;
use std::io::Write;

use crate::agent_model_control::PersistentAgentModelControl;
use crate::config;
use crate::runtime_reconcile::{
    DesiredSource, RuntimeExtensionsRefresh, RuntimeMcpControl, RuntimeReconciler, SourceId,
    adapt_prepared_bundle_skills,
};
use crate::{InstalledBundleRefresh, bundle_registry_path, plugins};

/// Agent catalog for a process before installed bundles are refreshed.
///
/// Rust-native built-in agents and the build-prepared first-party WorkflowBundle
/// are published together. Installed rows are merged later by the root refresh.
/// The first-party payload is decoded and verified before the runtime starts.
pub fn builtin_agent_catalog() -> anyhow::Result<Arc<AgentCatalog>> {
    let first_party = crate::installed_bundle_refresh::first_party_catalogs()
        .context("decode embedded first-party bundles")?;
    let first_party_refs = first_party.iter().collect::<Vec<_>>();
    let bundles = BundleCatalog::from_verified_catalogs(&first_party_refs)
        .context("build first-party bundle catalog")?;
    let catalog =
        AgentCatalog::new(Arc::new(bundles)).context("build agent catalog over built-ins")?;
    Ok(Arc::new(catalog))
}

/// Host identity sent to plugins during `initialize` (`name` + crate version).
pub fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: hya_version::BACKEND_VERSION.to_string(),
    }
}

/// Dev/offline provider router and model id when no live config is usable.
///
/// The router claims only `hya/offline`; an explicit unknown override remains
/// unknown and therefore fails through the normal typed routing error.
pub fn offline_router(model_override: Option<String>) -> (ProviderRouter, String) {
    let router = ProviderRouter::new().with(Arc::new(DevProvider::new()));
    (
        router,
        model_override.unwrap_or_else(|| "hya/offline".to_string()),
    )
}

pub(crate) fn process_owner_run_id() -> OwnerRunId {
    static OWNER_RUN_ID: OnceLock<OwnerRunId> = OnceLock::new();
    *OWNER_RUN_ID.get_or_init(OwnerRunId::new)
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
    io_tasks: Option<tokio_util::task::TaskTracker>,
    command: Option<Vec<String>>,
    staging_root: PathBuf,
    #[cfg(test)]
    terminate_notify: Option<Arc<tokio::sync::Notify>>,
}

impl BundleSidecarEnvironment {
    #[cfg(test)]
    fn from_command(command: Vec<String>, staging_root: PathBuf) -> Self {
        Self {
            io_tasks: hya_mcp::io_tasks::current(),
            command: Some(command),
            staging_root,
            terminate_notify: None,
        }
    }

    fn production() -> Self {
        let registry_path = bundle_registry_path();
        let staging_root = registry_path.parent().map_or_else(
            || PathBuf::from("activations"),
            |parent| parent.join("activations"),
        );
        Self {
            io_tasks: hya_mcp::io_tasks::current(),
            command: plugins::bundle_sidecar_command(),
            staging_root,
            #[cfg(test)]
            terminate_notify: None,
        }
    }
}

impl SidecarEnvironment for BundleSidecarEnvironment {
    fn factory_for(
        &self,
        binding: &TurnBinding,
        stable_agent_id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
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
            io_tasks: self.io_tasks.clone(),
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
    io_tasks: Option<tokio_util::task::TaskTracker>,
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

        let definition = self
            .binding
            .resolve_agent(&self.stable_agent_id)
            .ok_or_else(|| CoreError::AgentDefinitionMissing {
                agent_id: self.stable_agent_id.clone(),
            })?;
        let files = crate::agent_model_config::AgentModelConfigFiles::new(
            crate::config::active_config_path(),
        );
        let config_file = files
            .path_for_binding(&self.binding, definition.origin)
            .and_then(|path| std::path::absolute(path).map_err(Into::into))
            .map_err(|error| {
                CoreError::Invalid(format!("resolve bundle configuration path: {error}"))
            })?;
        let config_dir = config_file.parent().ok_or_else(|| {
            CoreError::Invalid("bundle configuration path has no parent".to_string())
        })?;
        let environment = BTreeMap::from([
            (
                crate::bundle_config::HYA_BUNDLE_CONFIG_DIR.to_string(),
                config_dir.to_string_lossy().into_owned(),
            ),
            (
                crate::bundle_config::HYA_BUNDLE_CONFIG_FILE.to_string(),
                config_file.to_string_lossy().into_owned(),
            ),
        ]);
        let lifecycle = match start.lifecycle {
            SidecarLifecycle::Transient => ActivationLifecycle::Transient,
            SidecarLifecycle::Resident => ActivationLifecycle::Resident,
        };
        let spec = PluginSpec {
            id: self.stable_agent_id.clone(),
            kind: PluginKindWire::Bun,
            command,
            timeout_ms: None,
            env: environment,
            plugin_dir: None,
            launch: ProcessLaunch::Bundle {
                root: activation_dir.path().to_path_buf(),
                activation: Some(ActivationMetadata {
                    activation_id: start.activation_id.clone(),
                    lifecycle,
                }),
            },
        };
        let host = Arc::new(
            hya_mcp::io_tasks::scope(
                self.io_tasks.clone(),
                PluginHost::connect(spec, host_info(), None),
            )
            .await
            .map_err(|error| CoreError::Invalid(format!("initialize bundle sidecar: {error}")))?,
        );
        let plugin = host.prepared_plugins().remove(0);
        let contributions = plugin.contributions();
        let tools_and_hooks = (|| {
            validate_bundle_sidecar_hooks(
                &self.binding,
                &self.stable_agent_id,
                &contributions.hooks,
            )?;
            validate_bundle_sidecar_skills(&self.binding, &self.stable_agent_id, contributions)?;
            let tools = bind_bundle_sidecar_tools(&self.binding, &self.stable_agent_id, &plugin)?;
            if !contributions.apis.is_empty() {
                return Err(CoreError::Invalid(
                    "bundle sidecar has undeclared API exports".into(),
                ));
            }
            if !contributions.workspace_adapters.is_empty() {
                tracing::warn!(agent = %self.stable_agent_id, "ignoring unsupported bundle sidecar workspace adapters");
            }
            let hooks = self
                .binding
                .sidecar_hook_view(&self.stable_agent_id, host.clone())?;
            Ok::<_, CoreError>((tools, hooks))
        })();
        let (tools, hooks) = match tools_and_hooks {
            Ok(bound) => bound,
            Err(error) => {
                let _ = host.terminate().await;
                return Err(error);
            }
        };

        Ok(Box::new(BundleSidecarHandle {
            loss: plugin
                .loss_token()
                .ok_or_else(|| CoreError::Invalid("private process has no loss token".into()))?,
            host: Some(host),
            activation_dir: Some(activation_dir.disarm()),
            tools,
            hooks,
            #[cfg(test)]
            terminate_notify: self.terminate_notify.clone(),
        }))
    }
}

struct BundleSidecarHandle {
    host: Option<Arc<PluginHost>>,
    loss: tokio_util::sync::CancellationToken,
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
        if let Some(host) = self.host.take() {
            host.stop_now();
        }
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
        if let Some(host) = self.host.take()
            && let Err(error) = host.shutdown().await
        {
            shutdown_error = Some(CoreError::Invalid(format!(
                "shutdown bundle sidecar: {error}"
            )));
        }

        let cleanup_error = self.cleanup_activation_dir();

        shutdown_error.or(cleanup_error).map_or(Ok(()), Err)
    }

    fn is_healthy(&self) -> bool {
        self.host.is_some() && !self.loss.is_cancelled()
    }

    fn loss_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        Some(self.loss.clone())
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        let mut terminate_error = None;
        if let Some(host) = self.host.take()
            && let Err(error) = host.terminate().await
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

fn bind_bundle_sidecar_tools(
    binding: &TurnBinding,
    stable_agent_id: &str,
    plugin: &PreparedPlugin,
) -> Result<Arc<[ResolvedTool]>, CoreError> {
    let (bundle_id, _) = binding
        .bundle_catalog()
        .resolve_agent_entry(stable_agent_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_agent_id.to_string(),
        })?;
    let policy = binding.agent_resource_policy(stable_agent_id)?;
    let selected = policy
        .selected_bundle_tool_ids()
        .iter()
        .map(|stable_id| {
            binding
                .bundle_catalog()
                .resolve_resource_entry(bundle_id, hya_bundle::ExportKind::Tool, stable_id)
                .map(|(_, resource)| resource)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let expected =
        bundle_sidecar_module_resources(binding, stable_agent_id, hya_bundle::ExportKind::Tool)?;
    let declarations = &plugin.contributions().tools;
    if expected.len() != declarations.len() {
        return Err(CoreError::Invalid(format!(
            "bundle sidecar tool declaration count mismatch: expected {}, got {}",
            expected.len(),
            declarations.len()
        )));
    }

    let mut bound = Vec::with_capacity(expected.len());
    for resource in &expected {
        declarations
            .iter()
            .find(|declaration| declaration.name == resource.local_id)
            .ok_or_else(|| {
                CoreError::Invalid(format!(
                    "missing bundle sidecar tool declaration `{}`",
                    resource.local_id
                ))
            })?;
    }
    for resource in selected {
        bound.push(ResolvedTool {
            tool: plugin
                .tool(&resource.local_id, &resource.stable_id)
                .ok_or_else(|| {
                    CoreError::Invalid(format!(
                        "missing bundle sidecar tool `{}`",
                        resource.local_id
                    ))
                })?,
            permission: ToolPermission::Tool,
        });
    }
    Ok(Arc::from(bound))
}

/// The complete declaration of the loaded modules, independent of an agent's view.
fn bundle_sidecar_module_resources<'a>(
    binding: &'a TurnBinding,
    stable_agent_id: &str,
    kind: hya_bundle::ExportKind,
) -> Result<Vec<&'a hya_bundle::PreparedResource>, CoreError> {
    let policy = binding.agent_resource_policy(stable_agent_id)?;
    let (bundle_id, _) = binding
        .bundle_catalog()
        .resolve_agent_entry(stable_agent_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_agent_id.to_string(),
        })?;
    let mut paths = BTreeSet::new();
    for (kind, ids) in [
        (
            hya_bundle::ExportKind::Tool,
            policy.selected_bundle_tool_ids(),
        ),
        (hya_bundle::ExportKind::Hook, policy.canonical_hook_ids()),
    ] {
        for id in ids {
            let (_, resource) = binding
                .bundle_catalog()
                .resolve_resource_entry(bundle_id, kind, id)?;
            paths.insert(resource.source_path.as_str());
        }
    }
    Ok(binding
        .bundle_catalog()
        .bundle_resources(bundle_id, kind)
        .into_iter()
        .flatten()
        .filter(|resource| paths.contains(resource.source_path.as_str()))
        .collect())
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
    let mut expected = BTreeSet::new();
    for resource in
        bundle_sidecar_module_resources(binding, stable_agent_id, hya_bundle::ExportKind::Hook)?
    {
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

/// Require a bundle sidecar's Skill declaration set to match signed prepared bytes.
fn validate_bundle_sidecar_skills(
    binding: &TurnBinding,
    stable_agent_id: &str,
    contributions: &PluginContributionSet,
) -> Result<(), CoreError> {
    let policy = binding.agent_resource_policy(stable_agent_id)?;
    let bundle_id = policy.bundle_id().ok_or_else(|| {
        CoreError::Invalid(format!(
            "bundle sidecar agent `{stable_agent_id}` has no owning bundle"
        ))
    })?;
    let resources = binding
        .bundle_catalog()
        .bundle_resources(bundle_id, hya_bundle::ExportKind::Skill)
        .ok_or_else(|| {
            CoreError::Invalid(format!(
                "bundle `{bundle_id}` has no prepared Skill namespace"
            ))
        })?;
    adapt_prepared_bundle_skills(bundle_id, resources, contributions)
        .map(|_| ())
        .map_err(|error| CoreError::Invalid(format!("bundle sidecar Skill declaration: {error}")))
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

/// Built-in root-session agent id used when neither an explicit override nor
/// config `default_agent` selects one. Matches `serve`'s own fallback
/// (`ServerState.default_agent`, then this same built-in agent).
pub const BUILTIN_DEFAULT_AGENT_ID: &str = hya_core::MAIN_AGENT_ID;

/// Agent base only — for HTTP/SSE server and interactive TUI AppState assembly.
///
/// Bundle `prompt=None` keeps this base; per-turn server discovery appends
/// Environment + current workdir AGENTS + references. Baking those at startup
/// would duplicate AGENTS when guidance is also layered.
///
/// The workdir is empty: `hya serve` has no working directory (ADR-0024).
/// Every turn's AgentSpec takes its workdir from its session.
pub fn agent_base_with_model(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    AgentSpec {
        name: AgentName::new(BUILTIN_DEFAULT_AGENT_ID),
        model: ModelRef::new(model),
        system_prompt: HARNESS_AGENT_BASE.to_string(),
        workdir: PathBuf::new(),
        reasoning,
    }
}

/// The caller's working directory, absolute, for the client-side commands
/// (`hya exec`/`run`/`-p`/`loop`/`goal`) that work where the user runs them.
/// They name it explicitly as their session's workdir; the server never
/// falls back to its own cwd (ADR-0024).
fn caller_workdir() -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| std::path::absolute(cwd).ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Direct-mode agent (exec/RPC/goal): base + Environment + caller-cwd AGENTS.
///
/// These paths call `run_turn` without a separate guidance layer, so context
/// must remain composed into `system_prompt` here.
pub fn agent_with_model(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    let workdir = caller_workdir();
    let env = PromptEnv {
        cwd: workdir.to_string_lossy().into_owned(),
        platform: std::env::consts::OS.to_string(),
        date: today(),
    };
    let context = discover_context_files(&workdir);
    let system_prompt = build_system_prompt(HARNESS_AGENT_BASE, &env, &context);
    AgentSpec {
        name: AgentName::new(BUILTIN_DEFAULT_AGENT_ID),
        model: ModelRef::new(model),
        system_prompt,
        workdir,
        reasoning,
    }
}

/// `--pure` variant of [`agent_with_model`]: same Environment block, but no
/// external AGENTS/context files are discovered or baked into the prompt.
pub fn agent_with_model_pure(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    let workdir = caller_workdir();
    let env = PromptEnv {
        cwd: workdir.to_string_lossy().into_owned(),
        platform: std::env::consts::OS.to_string(),
        date: today(),
    };
    let system_prompt = build_system_prompt(HARNESS_AGENT_BASE, &env, &[]);
    AgentSpec {
        name: AgentName::new(BUILTIN_DEFAULT_AGENT_ID),
        model: ModelRef::new(model),
        system_prompt,
        workdir,
        reasoning,
    }
}

/// Resolve which agent id a headless root session (`exec`/`run`, `-p` goal
/// mode, `loop`, `rpc`) should use, mirroring `serve`'s own precedence:
/// explicit override (a CLI flag, when the caller has one) first, then config
/// `default_agent`, then the built-in [`BUILTIN_DEFAULT_AGENT_ID`].
///
/// Unlike `serve` (which resolves lazily per HTTP session-create request and
/// can silently keep serving other sessions on failure), a headless run has
/// exactly one root session: an unresolvable candidate fails the whole
/// invocation up front with the same typed error `serve` uses
/// (`BundleError::UnknownAgentId`) rather than silently falling back to the
/// built-in agent.
///
/// # Errors
/// Propagates [`SessionEngine::bind_root_runtime`] failures, and returns
/// `CoreError::Bundle(BundleError::UnknownAgentId)` when the resolved
/// candidate is not a selectable agent in the bound catalog.
pub async fn resolve_headless_agent_name(
    engine: &SessionEngine,
    workdir: &Path,
    explicit: Option<&str>,
    default_agent: Option<&str>,
) -> Result<AgentName, CoreError> {
    let candidate = explicit
        .or(default_agent)
        .unwrap_or(BUILTIN_DEFAULT_AGENT_ID);
    let binding = engine.bind_root_runtime(workdir).await?;
    binding
        .resolve_agent(candidate)
        .map(|definition| AgentName::new(definition.stable_id))
        .ok_or_else(|| {
            CoreError::from(hya_bundle::BundleError::UnknownAgentId {
                agent_id: candidate.to_string(),
            })
        })
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
    /// Live model routes (or the canonical offline provider).
    pub router: ProviderRouter,
    /// Shared immutable startup model/provider catalog.
    pub catalog: Arc<ProviderCatalogSnapshot>,
    /// Active request model id for new sessions.
    pub model: String,
    /// MCP server configs to connect at engine build.
    pub mcp: BTreeMap<String, McpServerConfig>,
    /// Plugin specs from `config.yaml` (process-wide). Project plugins
    /// (`<root>/.hya/plugins`) load per Project at its first bind.
    pub plugins: Vec<PluginSpec>,
    /// Preferred primary agent when workdir does not select one.
    pub default_agent: Option<String>,
    /// Set when no usable config or live provider was found.
    /// Interactive startup emits it; headless/machine-readable modes ignore it.
    pub offline_notice: Option<OfflineNotice>,
    /// Tool permission policy for the engine.
    pub permission: InvocationPolicy,
    /// Web-search plane configuration.
    pub websearch: WebSearchConfig,
    /// Empty-`models` providers awaiting background discovery refresh.
    pub pending_discovery: Vec<crate::config::PendingCatalogDiscovery>,
    /// `--pure`: load no external AGENTS.md context, MCP servers, or plugins.
    /// Websearch keeps its own configuration; builtin tools and the embedded
    /// skill catalog are unaffected.
    pub pure: bool,
    /// Parsed startup settings for engine composition.
    pub settings: config::EngineSettings,
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

    /// `--pure` mode: load no external MCP servers or plugins (config is
    /// cleared), and mark the runtime so engine builds and per-turn guidance
    /// skip external context. Websearch keeps its own configuration; builtin
    /// tools and the embedded skill catalog are unaffected.
    #[must_use]
    pub fn with_pure(mut self, pure: bool) -> Self {
        if pure {
            self.mcp.clear();
            self.plugins.clear();
        }
        self.pure = pure;
        self
    }
}
fn offline_runtime(model_override: Option<String>, strict: bool) -> RuntimeConfig {
    let (router, model) = offline_router(model_override);
    let catalog = Arc::new(ProviderCatalogSnapshot::build(
        Vec::new(),
        Vec::new(),
        Some(ModelRef::new("hya/offline")),
    ));
    let router = router.with_catalog_snapshot(Arc::clone(&catalog));
    RuntimeConfig {
        router,
        catalog,
        model,
        mcp: BTreeMap::new(),
        plugins: Vec::new(),
        default_agent: None,
        offline_notice: Some(OfflineNotice {
            config_path: config::expected_config_path(),
        }),
        permission: if strict {
            InvocationPolicy::default().with_model(PermissionModel::Strict)
        } else {
            InvocationPolicy::default()
        },
        websearch: WebSearchConfig::default(),
        pending_discovery: Vec::new(),
        settings: config::EngineSettings::default(),
        pure: false,
    }
}

/// Resolve providers and the immutable catalog before returning runtime state.
/// A configuration error falls back to the offline provider (with a notice).
pub async fn resolve_runtime(model_override: Option<String>) -> RuntimeConfig {
    match resolve_runtime_strict(model_override.clone()).await {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hya: config error ({error:#}); using the offline provider");
            offline_runtime(model_override, true)
        }
    }
}

/// [`resolve_runtime`] without the offline fallback: a configuration error
/// is returned rather than composing the offline provider.
///
/// # Errors
///
/// The configuration could not be loaded.
async fn resolve_runtime_strict(model_override: Option<String>) -> anyhow::Result<RuntimeConfig> {
    let Some(cfg) = config::load().await? else {
        return Ok(offline_runtime(model_override, false));
    };
    let model = model_override
        .or_else(|| std::env::var("HYA_MODEL").ok())
        .unwrap_or_else(|| cfg.default_model.clone());
    let offline_notice = cfg.catalog.notice().map(|_| OfflineNotice {
        config_path: config::expected_config_path(),
    });
    Ok(RuntimeConfig {
        router: cfg.router,
        catalog: cfg.catalog,
        model,
        mcp: cfg.mcp,
        plugins: plugins::resolve(cfg.plugins),
        default_agent: cfg.default_agent,
        offline_notice,
        permission: cfg.permission,
        websearch: cfg.websearch,
        pending_discovery: cfg.pending_discovery,
        settings: cfg.settings,
        pure: false,
    })
}

/// Reserve ownership before opening a SQLite runtime store, or use memory when `db` is empty.
pub async fn open_store(db: &str) -> anyhow::Result<SessionStore> {
    if db.is_empty() {
        SessionStore::connect_memory()
            .await
            .context("open in-memory store")
    } else {
        SessionStore::connect_runtime(db)
            .await
            .with_context(|| format!("open store at {db}"))
    }
}

struct ResolvedSpawnMember {
    request: SpawnMember,
    authorized_target: AgentName,
    agent: AgentSpec,
    binding: TurnBinding,
    agents: Arc<[hya_tool::AgentDef]>,
    resources: AgentResourcePolicy,
    sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
    /// Immutable guidance Arc cloned from the spawn request (not from disk).
    guidance: Option<Arc<str>>,
}

/// Pre-admission team-root context for main-as-actor synthesis on resident batches.
///
/// Bound from the spawn TurnBinding + root projection stable AgentName — never from
/// a nested caller's roster/resource policy.
struct MainActivationContext {
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
        .read_projection_shared(root)
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
        agent,
        binding: binding.clone(),
        agents,
        resources,
        guidance,
    })
}

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

fn resolve_spawn_member(
    ctx: &ResolveSpawnMemberCtx<'_>,
    member: SpawnMember,
) -> Result<ResolvedSpawnMember, SpawnError> {
    let definition = authorize_spawn_target(ctx.binding, ctx.allowed_agents, ctx.caller, &member)?;
    resolve_authorized_spawn_member(ctx, member, &definition)
}

/// Resolve a member's caller-supplied model request against the live catalog.
///
/// Branch 1 (exact valid id) and branch 2 (first substring match, bare
/// vendor ids excluded) replace the request with the resolved id; anything
/// else clears it so [`apply_spawn_model_policy`] falls through to the
/// definition/preference chain. A `#effort` suffix on the request is split
/// off before matching and becomes the member's `effort` unless the call
/// also passed an explicit `effort`, which wins.
fn resolve_member_dispatch_model(
    ctx: &ResolveSpawnMemberCtx<'_>,
    mut member: SpawnMember,
) -> SpawnMember {
    let catalog = ctx.engine.provider_catalog();
    let model_ids: Vec<String> = catalog
        .iter()
        .map(|row| format!("{}/{}", row.provider_id, row.model_id))
        .collect();
    let provider_ids: Vec<String> = ctx
        .engine
        .provider_catalog_snapshot()
        .providers()
        .iter()
        .map(|state| state.provider_id.clone())
        .collect();
    let requested = member
        .model
        .clone()
        .or_else(|| {
            member
                .inline_agent
                .as_ref()
                .and_then(|inline| inline.model.clone())
        })
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let Some(requested) = requested else {
        return member;
    };
    let (requested, suffix) = match requested.rsplit_once('#') {
        Some((base, effort)) => (base.to_owned(), Some(effort.to_owned())),
        None => (requested, None),
    };
    if member
        .effort
        .as_deref()
        .is_none_or(|effort| effort.trim().is_empty())
    {
        member.effort = suffix.filter(|effort| !effort.trim().is_empty());
    }
    let resolved = resolve_dispatch_model(&requested, &model_ids, &provider_ids)
        .map(|model| model.to_string());
    member.model = resolved.clone();
    if let Some(inline) = member.inline_agent.as_mut() {
        inline.model = resolved;
    }
    member
}

/// Apply a spawn-time `effort` as the child model's `#effort` suffix.
///
/// The suffix is the effort resolver's highest layer, so it outranks the
/// Agent's configured/remembered default and authored policy. An unknown label,
/// or one the child's catalog row does not advertise, is rejected (`none` is
/// always allowed). An absent or empty `effort` leaves the Agent untouched.
fn apply_spawn_effort(
    engine: &SessionEngine,
    mut agent: AgentSpec,
    effort: Option<&str>,
) -> Result<AgentSpec, SpawnError> {
    let Some(label) = effort.map(str::trim).filter(|label| !label.is_empty()) else {
        return Ok(agent);
    };
    let base = agent
        .model
        .as_str()
        .rsplit_once('#')
        .map_or(agent.model.as_str(), |(base, _)| base)
        .to_owned();
    let invalid = || SpawnError::InvalidEffort {
        effort: label.to_owned(),
        model: base.clone(),
    };
    let parsed = ReasoningEffort::parse(label).ok_or_else(invalid)?;
    if parsed != ReasoningEffort::Off {
        let advertised = engine
            .provider_catalog()
            .into_iter()
            .find(|row| {
                base == format!("{}/{}", row.provider_id, row.model_id) || base == row.model_id
            })
            .map(|row| row.reasoning_variants)
            .filter(|variants| !variants.is_empty());
        if let Some(variants) = advertised
            && !variants
                .iter()
                .any(|variant| ReasoningEffort::parse(variant) == Some(parsed))
        {
            return Err(invalid());
        }
    }
    agent.model = ModelRef::new(format!("{base}#{label}"));
    Ok(agent)
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
    let agent = ctx
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

    let agent = apply_agent_model_preference(
        agent,
        definition,
        ctx.binding.agent_model_preference(definition.stable_id),
        ctx.is_servable,
    );
    // Dispatch-time model resolution: a caller-supplied model id is resolved
    // against the live catalog (exact -> substring, bare vendor ids never
    // substring-dispatch). Unresolvable requests defer to the user's
    // configured chain instead of overriding it verbatim.
    let member = resolve_member_dispatch_model(ctx, member);
    let agent =
        apply_spawn_model_policy(agent, definition, &member, ctx.categories, ctx.is_servable);
    let mut agent = apply_spawn_effort(ctx.engine, agent, member.effort.as_deref())?;
    if let Some(inline) = member.inline_agent.as_ref() {
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
        sidecar_factory,
        guidance: ctx.guidance.clone(),
    })
}

fn authorize_spawn_target<'a>(
    binding: &'a TurnBinding,
    allowed_agents: &[hya_tool::AgentDef],
    caller: &str,
    member: &SpawnMember,
) -> Result<AgentDefinition<'a>, SpawnError> {
    let requested = member.subagent_type.trim();
    let requested = if requested.is_empty() {
        hya_core::TASK_AGENT_ID
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

pub use hya_server::RuntimeTaskOwner;

/// Public aggregate returned by [`build_session_engine`].
///
/// Owns engine products plus composition services and request handlers. Not `Clone`.
/// Callers must keep this value until they finish using the engine and then call
/// [`shutdown`](BuiltSessionEngine::shutdown) (or drop for a nonblocking fallback).
#[must_use]
pub struct BuiltSessionEngine {
    engine: Arc<SessionEngine>,
    resident_supervisor: Arc<ResidentSupervisor>,
    workflow_control: crate::WorkflowControl,
    agent_model_control: PersistentAgentModelControl,
    asks: Option<tokio::sync::mpsc::UnboundedReceiver<AskRequest>>,
    questions: Option<tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>>,
    mcp_control: Arc<dyn hya_server::McpControl>,
    lifecycle: RuntimeTaskOwner,
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

    /// Shared durable Workflow control adapter for direct and server surfaces.
    #[must_use]
    pub fn workflow_control(&self) -> crate::WorkflowControl {
        self.workflow_control.clone()
    }

    /// Shared durable Agent model preference control for backend and server surfaces.
    #[must_use]
    pub fn agent_model_control(&self) -> PersistentAgentModelControl {
        self.agent_model_control.clone()
    }

    /// Resolve the effective model for a new root Session without an explicit override.
    ///
    /// The supplied workdir binds one current Agent catalog/preference snapshot.
    /// Configured direct/category policy and an eligible remembered model are
    /// applied before the process base model.
    ///
    /// # Errors
    /// Returns runtime binding, unknown-Agent, or model-control failures.
    pub async fn effective_root_model(
        &self,
        agent: &AgentSpec,
        workdir: &Path,
    ) -> anyhow::Result<ModelRef> {
        let binding = self
            .engine
            .bind_root_runtime(workdir)
            .await
            .context("bind root runtime for Agent model resolution")?;
        self.agent_model_control
            .effective_model(&binding, agent.name.as_str(), &agent.model)
            .context("resolve effective root Agent model")
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

    /// Admit every daemon restore before starting recovered resident episodes.
    /// The restore turn owns its root lease before recovered child mail can wake main.
    pub async fn resume_interrupted(
        &self,
        state: &hya_server::AppState,
    ) -> Result<Vec<SessionId>, CoreError> {
        let restored = state.resume_interrupted_turns().await;
        let residents = self.resident_supervisor.start_recovered().await?;
        crate::startup_trace::mark("residents_recovered", Some(&residents.to_string()));
        Ok(restored)
    }

    /// Graceful stop of every in-flight turn in every session (see
    /// [`ResidentSupervisor::drain`]): new turns are refused, active turns are
    /// cancelled with `cause` and given [`DRAIN_DEADLINE`] to close their
    /// messages, then team members are made terminal and each lead is parked.
    /// Stops and joins composition intake and registrations before draining
    /// residents. The owned future can be passed to a server shutdown callback.
    /// Idempotent; the first cause wins.
    pub fn drain(
        &self,
        cause: FinishCause,
    ) -> impl std::future::Future<Output = TurnDrainReport> + Send + 'static + use<> {
        let engine = self.engine.clone();
        let residents = self.resident_supervisor.clone();
        let shutdown = self.lifecycle.shutdown();
        let io_tasks = self.lifecycle.io_tasks.clone();
        async move {
            engine.begin_drain(cause);
            shutdown.await;
            let report = residents.drain(cause, DRAIN_DEADLINE).await;
            engine.runtime_registry().shutdown_sources().await;
            io_tasks.close();
            io_tasks.wait().await;
            report
        }
    }

    /// Stop intake, cancel and join admitted handlers, then drain turns and
    /// residents so no late registration can escape the drain. Safe to call repeatedly.
    pub async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.drain(FinishCause::Shutdown).await;
        Ok(())
    }
}

/// Adapt the app-owned Workflow control to the dependency-inverted server port.
impl hya_server::WorkflowControl for crate::WorkflowControl {
    fn execute(
        &self,
        session: SessionId,
        command: hya_proto::WorkflowCommand,
        delivery: WorkflowDelivery,
    ) -> hya_server::WorkflowControlFuture<'_> {
        Box::pin(async move {
            crate::WorkflowControl::execute(
                self,
                session,
                crate::WorkflowInvocation {
                    delivery,
                    ..crate::WorkflowInvocation::default()
                },
                command,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .map_err(|error| hya_server::WorkflowControlError::new(error.code(), error.to_string()))
        })
    }

    fn decorate(
        &self,
        session: SessionId,
        state: hya_proto::WorkflowProjection,
    ) -> hya_server::WorkflowDecorationFuture<'_> {
        Box::pin(async move {
            crate::WorkflowControl::decorate(self, session, state)
                .await
                .map_err(|error| {
                    hya_server::WorkflowControlError::new(error.code(), error.to_string())
                })
        })
    }

    fn list(
        &self,
        scope: Option<std::path::PathBuf>,
    ) -> futures::future::BoxFuture<
        '_,
        Result<Vec<hya_proto::WorkflowSummary>, hya_server::WorkflowControlError>,
    > {
        Box::pin(async move {
            crate::WorkflowControl::list(self, scope.as_deref())
                .await
                .map_err(|error| {
                    hya_server::WorkflowControlError::new(error.code(), error.to_string())
                })
        })
    }

    fn active_run(&self, session: SessionId) -> Option<hya_proto::WorkflowRunId> {
        crate::WorkflowControl::active_run(self, session)
            .ok()
            .flatten()
    }

    fn wait_idle(
        &self,
        session: SessionId,
    ) -> futures::future::BoxFuture<'_, Result<(), hya_server::WorkflowControlError>> {
        Box::pin(async move {
            self.wait_run_idle(session).await.map_err(|error| {
                hya_server::WorkflowControlError::new(error.code(), error.to_string())
            })
        })
    }

    fn cancel(&self, session: SessionId) -> bool {
        self.cancel_run(session)
    }
}

/// Start recovered residents, run a body, and always attempt supervisor shutdown.
/// Server embedders admit restore prompts with `resume_interrupted` first.
pub async fn with_built_session_engine<T, E, F, Fut>(
    mut built: BuiltSessionEngine,
    body: F,
) -> Result<T, E>
where
    F: FnOnce(&mut BuiltSessionEngine) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: From<CoreError>,
{
    let body_result = match built.resident_supervisor.start_recovered().await {
        Ok(_) => body(&mut built).await,
        Err(error) => Err(E::from(error)),
    };
    let shutdown_result = built.shutdown().await.map_err(E::from);
    match (body_result, shutdown_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Spawn the background task that admits multi-agent team/member spawn requests.
///
/// Keep the returned owner alive; call its `shutdown().await` before teardown.
pub fn spawn_team_supervisor(
    rx: tokio::sync::mpsc::Receiver<BoundSpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    categories: Arc<CategoryRegistry>,
    resident_supervisor: Arc<ResidentSupervisor>,
) -> RuntimeTaskOwner {
    let owner = RuntimeTaskOwner::new();
    let sidecar_environment = Arc::new(BundleSidecarEnvironment {
        io_tasks: Some(owner.io_tasks.clone()),
        ..BundleSidecarEnvironment::production()
    });
    let lifecycle = spawn_team_supervisor_with_environment(
        rx,
        engine.clone(),
        base,
        categories,
        resident_supervisor.clone(),
        sidecar_environment,
        RuntimeTaskOwner::new(),
    );
    // The standalone owner must perform the same ordered teardown as a built engine.
    let stop = owner.stop.clone();
    let io_tasks = owner.io_tasks.clone();
    owner.tasks.spawn(async move {
        stop.cancelled().await;
        engine.begin_drain(FinishCause::Shutdown);
        lifecycle.shutdown().await;
        resident_supervisor
            .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
            .await;
        engine.runtime_registry().shutdown_sources().await;
        io_tasks.close();
        io_tasks.wait().await;
    });
    owner
}

/// Receive Workflow requests without waiting for another Session's run.
fn spawn_workflow_supervisor(
    mut rx: tokio::sync::mpsc::Receiver<BoundWorkflowRequest>,
    control: crate::WorkflowControl,
    stop: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let tasks = control.tasks.clone();
    let slots = Arc::new(tokio::sync::Semaphore::new(rx.max_capacity()));
    tasks.spawn(async move {
        loop {
            let bound_request = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                request = rx.recv() => match request {
                    Some(request) => request,
                    None => break,
                },
            };
            let (binding, request) = bound_request.into_parts();
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                let _ = request.reply.send(Err(hya_tool::WorkflowHostError::new(
                    "WORKFLOW_OVERLOADED",
                    "workflow host queue is full; retry shortly",
                )));
                continue;
            };
            let control = control.clone();
            control.tasks.clone().spawn(async move {
                handle_workflow_request(&control, binding, request, slot).await;
            });
        }
    })
}

/// Execute one framed workflow request through the app-owned control seam.
async fn handle_workflow_request(
    control: &crate::WorkflowControl,
    binding: TurnBinding,
    request: hya_tool::WorkflowRequest,
    slot: tokio::sync::OwnedSemaphorePermit,
) {
    let hya_tool::WorkflowRequest {
        parent,
        operation,
        command,
        cancel,
        reply,
    } = request;
    let result = control
        .execute_with_admission(
            parent,
            crate::WorkflowInvocation {
                caller: None,
                operation: Some(operation),
                binding: Some(binding),
                delivery: WorkflowDelivery::Finished,
            },
            command,
            cancel,
            Some(slot),
        )
        .await
        .map_err(|error| hya_tool::WorkflowHostError::new(error.code(), error.to_string()));
    let _ = reply.send(result);
}

fn spawn_team_supervisor_with_environment(
    mut rx: tokio::sync::mpsc::Receiver<BoundSpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    categories: Arc<CategoryRegistry>,
    resident_supervisor: Arc<ResidentSupervisor>,
    sidecar_environment: Arc<BundleSidecarEnvironment>,
    lifecycle: RuntimeTaskOwner,
) -> RuntimeTaskOwner {
    let stop_child = lifecycle.stop.clone();
    let tasks = lifecycle.tasks.clone();
    lifecycle.tasks.spawn(async move {
        loop {
            let bound_request = tokio::select! {
                biased;
                _ = stop_child.cancelled() => break,
                bound_request = rx.recv() => match bound_request {
                    Some(request) => request,
                    None => break,
                },
            };
            let (binding, req) = bound_request.into_parts();
            let parent = match engine.read_projection_shared(req.parent).await {
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
            let router = engine.provider_router();
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
            // Every batch needs team-root main activation context resolved from
            // the same captured TurnBinding before any durable admission
            // (ADR-0015: all members are residents). Nested callers must not
            // supply their own roster/resource policy for main.
            let main_activation = {
                match resolve_main_activation_context(
                    &engine,
                    &binding,
                    &base,
                    req.parent,
                    request_guidance.clone(),
                )
                .await
                {
                    Ok(context) => context,
                    Err(error) => {
                        let _ = req.reply.send(Err(error));
                        continue;
                    }
                }
            };
            match engine.session_lineage(req.parent).await {
                Ok((_, depth)) if depth.saturating_add(1) > hya_core::MAX_SUBAGENT_DEPTH => {
                    let _ = req.reply.send(Err(SpawnError::Overloaded));
                    continue;
                }
                Err(error) => {
                    eprintln!("hya: failed to resolve spawn lineage ({error})");
                    let _ = req.reply.send(Err(SpawnError::Unavailable));
                    continue;
                }
                Ok(_) => {}
            }
            if req.cancel.is_cancelled() {
                let _ = req.reply.send(Err(SpawnError::Cancelled));
                continue;
            }
            let actor_claim = req.operation.actor_claim();
            let source_tool_call = req.operation.source_tool_call_id();
            let resident_supervisor = resident_supervisor.clone();
            let stop = stop_child.clone();
            tasks.spawn(async move {
                let cancel = req.cancel.clone();
                let execution = async move {
                    let main = main_activation;
                    let main = hya_core::resident::MainActivation {
                        agent: main.agent,
                        binding: main.binding,
                        agents: main.agents,
                        resources: main.resources,
                        guidance: main.guidance,
                    };
                    let models = resolved
                        .iter()
                        .map(|member| member.agent.model.to_string())
                        .collect::<Vec<_>>();
                    let members = resolved
                        .into_iter()
                        .map(|resolved| hya_core::resident::TaskResidentSpawn {
                            agent: resolved.agent,
                            runtime: (
                                resolved.binding,
                                resolved.agents,
                                resolved.resources,
                                resolved.sidecar_factory,
                            ),
                            directive: resolved.request.prompt,
                            task: TaskSpawnOrigin {
                                subagent_type: resolved.authorized_target.as_str().into(),
                                description: resolved.request.description,
                                tool_call: Some(source_tool_call),
                            },
                            guidance: resolved.guidance,
                        })
                        .collect();
                    let result = resident_supervisor
                        .spawn_residents_typed(req.parent, members, main, actor_claim.as_ref())
                        .await;
                    let outcome = match result {
                        Ok(identities) => Ok(identities
                            .into_iter()
                            .zip(models)
                            .map(|((session, handle), model)| MemberOutcome {
                                member: handle.clone(),
                                session: session.to_string(),
                                status: "running".into(),
                                summary: format!(
                                    "Resident {handle} is live; results arrive as its report."
                                ),
                                model: Some(model),
                            })
                            .collect()),
                        Err(CoreError::Store(StoreError::CapacityExceeded { .. })) => {
                            Err(SpawnError::Overloaded)
                        }
                        Err(CoreError::Cancelled) => Err(SpawnError::Cancelled),
                        Err(error) => {
                            eprintln!("hya: atomic spawn registration failed ({error})");
                            Err(SpawnError::Unavailable)
                        }
                    };
                    let _ = req.reply.send(outcome);
                };
                tokio::pin!(execution);
                tokio::select! {
                    biased;
                    () = stop.cancelled() => {
                        cancel.cancel();
                        execution.await;
                    }
                    () = &mut execution => {}
                }
            });
        }
    });
    lifecycle
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
    /// `--pure`: the runtime registry serves builtin skills only and never
    /// reads external skill directories.
    pure: bool,
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
    tool_config: (WebSearchConfig, InvocationPolicy, config::EngineSettings),
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
            pure: false,
        },
    )
    .await
}

/// [`build_session_engine`] in `--pure` mode: identical wiring except the
/// runtime registry never discovers external skill directories (the builtin
/// embedded catalog is the whole skill surface).
pub async fn build_session_engine_pure(
    store: SessionStore,
    router: ProviderRouter,
    agent: &AgentSpec,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
    tool_config: (WebSearchConfig, InvocationPolicy, config::EngineSettings),
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
            pure: true,
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
    tool_config: (WebSearchConfig, InvocationPolicy, config::EngineSettings),
    options: EngineBuildOptions,
) -> anyhow::Result<BuiltSessionEngine> {
    let lifecycle = RuntimeTaskOwner::new();
    let owner_run_id = process_owner_run_id();
    store
        .claim_runtime_owner(owner_run_id)
        .context("claim runtime owner before startup recovery")?;
    // Crash recovery: close every assistant turn a dead process left open
    // (`MessageFinished { cancelled, cause: interrupted }`, open tool parts
    // errored, member rows cancelled) before any turn of this process runs.
    // Indexed, so it only touches sessions a crash left mid-turn.
    store
        .recover_interrupted_turns(owner_run_id)
        .await
        .context("close turns a stopped process left open")?;
    crate::startup_trace::mark("interrupted_turns_recovered", None);
    store
        .recover_nonterminal_workflows(owner_run_id, "backend startup recovery")
        .await
        .context("recover nonterminal Workflows before spawn readiness")?;
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
    crate::startup_trace::mark("store_recovery", None);
    let (websearch, invocation_policy, settings) = tool_config;
    let router = Arc::new(router);
    let registry = ToolRegistry::builtins();
    if !websearch.enabled {
        registry.remove("websearch");
    }

    let project_plugins = if options.pure {
        crate::project_plugins::ProjectPluginSettings::disabled()
    } else {
        let configured = settings.plugin_ids();
        crate::project_plugins::ProjectPluginSettings::new(host_info()).with_configured_ids(
            plugins
                .iter()
                .map(|spec| spec.id.clone())
                .filter(|id| !configured.contains(id)),
        )
    };
    let defer_mcp = options.defer_mcp && !mcp.is_empty();
    let catalog = builtin_agent_catalog()?;
    // The startup catalog holds only the first-party bundle, so its schema
    // rows come straight from the same prepared document.
    let first_party = crate::installed_bundle_refresh::first_party_catalogs()?;
    let schema_rows = first_party
        .iter()
        .flat_map(|catalog| catalog.schemas().to_vec())
        .collect::<Vec<_>>();
    let static_sources = crate::installed_bundle_refresh::static_bundle_skill_sources(
        catalog.bundles().as_ref(),
        &schema_rows,
    )?;
    let runtime = Arc::new(RuntimeRegistry::new(registry, catalog).with_pure_skills(options.pure));
    if !static_sources.is_empty() {
        runtime.refresh(|candidate| {
            candidate.replace_sources_of_kind(RuntimeSourceKind::Bundle, static_sources)
        })?;
    }
    let categories = Arc::new(settings.categories());
    let reconciler =
        Arc::new(RuntimeReconciler::new(runtime.clone()).with_io_tasks(lifecycle.io_tasks.clone()));
    let mcp_control = Arc::new(RuntimeMcpControl::new(reconciler.clone()));
    let catalog_refresh = Arc::new(RuntimeExtensionsRefresh::new(
        crate::ProjectScopeRefresh::new(Arc::new(
            InstalledBundleRefresh::new(bundle_registry_path())
                .with_io_tasks(lifecycle.io_tasks.clone())
                .with_host_reads(Arc::new(hya_core::StoreSessionReads::new(store.clone()))),
        ))
        .with_project_plugins(project_plugins),
        reconciler.clone(),
        options.pure,
    )?);

    let rules = PermissionRules::new(vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::Glob, "*", Mode::Allow),
        Rule::new(Action::Grep, "*", Mode::Allow),
    ]);
    let (permission, asks) = PermissionPlane::new_with_policy(rules, invocation_policy);
    let (interaction, questions) = InteractionPlane::new();
    let subagent_limits = settings.subagents();
    let spawn_queue_capacity = usize::try_from(subagent_limits.per_run_budget)
        .unwrap_or(tokio::sync::Semaphore::MAX_PERMITS)
        .clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(spawn_queue_capacity);
    let (workflow_sender, workflow_rx) = BoundWorkflowSender::with_capacity(spawn_queue_capacity);
    let (mailbox, mailbox_rx) = MailboxPlane::new();
    let (lifecycle_plane, lifecycle_rx) = LifecyclePlane::new();
    let summarizer: Arc<dyn Summarizer> =
        Arc::new(ModelSummarizer::new(router.clone(), agent.model.clone()));
    let bus = EventBus::new(crate::config::resolve_event_bus_capacity());
    let governor = SubagentGovernor::new(subagent_limits);
    let sidecar_environment = Arc::new(BundleSidecarEnvironment {
        io_tasks: Some(lifecycle.io_tasks.clone()),
        ..BundleSidecarEnvironment::production()
    });
    let context_settings = settings.context();
    let engine_builder = SessionEngine::new(store, router, runtime, permission, bus)
        .with_base_system_prompt(agent.system_prompt.clone())
        // Stop/start records use this store's runtime-owner claim.
        .with_runtime_owner(owner_run_id)
        .with_global_reasoning(settings.reasoning())
        .with_catalog_scope_cache(settings.scopes())
        .with_catalog_refresh(catalog_refresh)
        .with_sidecar_environment(sidecar_environment.clone())
        .with_model_categories(categories.clone())
        // Route `categories:` failover chains into the engine's cross-model
        // plane so turn-time pre-stream failures advance through the ordered
        // candidates instead of failing the whole turn.
        .with_model_fallbacks(category_model_fallbacks(&categories))
        .with_compaction(summarizer, context_settings.compaction)
        .with_token_accounting(TokenAccounting::new(context_settings.token_accounting))
        .with_formatter(settings.formatter())
        .with_lsp(settings.lsp()?)
        .with_websearch(WebSearchPlane::configured(websearch))
        .with_interaction(interaction)
        // Explicit cancellation closes intake: the engine owns each sender while
        // its receiver service retains the engine.
        .with_spawn_sender(spawn_sender)
        // INVARIANT (workflow-intake liveness): mirrors `with_spawn_sender` --
        // the engine owns this sender and the workflow worker owns an
        // `Arc<SessionEngine>`, so queued requests always find a live executor.
        .with_workflow_sender(workflow_sender)
        .with_mailbox(mailbox)
        .with_lifecycle(lifecycle_plane)
        .with_governor(governor);

    let engine = Arc::new(engine_builder);
    let (agent_configuration, agent_efforts) = settings.agents()?;
    let agent_model_control = PersistentAgentModelControl::load(
        engine.store().clone(),
        owner_run_id,
        engine.runtime_registry(),
        engine.clone(),
    )
    .await
    .context("load Agent model preferences before engine readiness")?
    .with_categories(categories.clone())
    .with_configuration_snapshot(
        crate::agent_model_config::AgentModelConfigFiles::new(crate::config::active_config_path()),
        agent_configuration,
    )
    .await;
    engine
        .runtime_registry()
        .publish_agent_effort_configuration(agent_efforts);
    let plugin_desired = plugins
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
        if let Err(error) = mcp_control
            .reconcile_plan(plugin_plan, lifecycle.stop.clone())
            .await
        {
            eprintln!("hya: plugin reconciliation rejected ({error})");
        }

        let mut desired = plugin_desired;
        desired.extend(mcp_desired);
        let deferred_plan = reconciler
            .replace_desired(desired)
            .context("plan deferred MCP reconciliation")?;
        let control_bg = mcp_control.clone();
        let stop = lifecycle.stop.clone();
        lifecycle.tasks.spawn(async move {
            if let Err(error) = control_bg.reconcile_plan(deferred_plan, stop).await {
                eprintln!("hya: MCP runtime refresh rejected ({error})");
            }
        });
    } else {
        let mut desired = plugin_desired;
        desired.extend(mcp_desired);
        let plan = reconciler
            .replace_desired(desired)
            .context("plan startup runtime reconciliation")?;
        if let Err(error) = mcp_control
            .reconcile_plan(plan, lifecycle.stop.clone())
            .await
        {
            eprintln!("hya: startup runtime reconciliation rejected ({error})");
        }
    }
    crate::startup_trace::mark("engine_runtime", None);
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
            .read_projection_shared(root)
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
            .read_projection_shared(actor_id)
            .await
            .context("replay recovered resident session")?;
        let workdir = actor_projection
            .session
            .workdir
            .as_ref()
            .map(PathBuf::from)
            .context("recovered resident session has no workdir")?;
        // Resume is not a new spawn: exact catalog lookup only (no can_spawn,
        // no legacy general/base synthesis). Missing or inline-only identity fails.
        let recorded = actor_projection
            .session
            .agent
            .as_ref()
            .unwrap_or(&entry.agent_type);
        // The resident works in its session's workdir, not the process's.
        let base = AgentSpec {
            workdir: workdir.clone(),
            ..agent.clone()
        };
        let recovered_binding = engine
            .bind_session_runtime(actor_id, &workdir)
            .await
            .context("bind recovered resident Session model configuration")?;
        let (recovered_agent, recovered_runtime) = engine
            .resolve_resident_runtime(recovered_binding, &base, recorded)
            .context("resolve recovered resident capabilities from current catalog")?;
        resident_supervisor.defer_recovered_resident(
            root,
            entry.handle,
            recovered_agent,
            recovered_runtime,
            recovered,
            report.work,
        );
    }
    let workflow_control = crate::WorkflowControl::new_with_routing(
        engine.clone(),
        agent.clone(),
        resident_supervisor.clone(),
        owner_run_id,
        categories.clone(),
        &lifecycle,
    );
    let lifecycle = spawn_team_supervisor_with_environment(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        categories,
        resident_supervisor.clone(),
        sidecar_environment,
        lifecycle,
    );
    // Serve `workflow` tool requests from a dedicated worker loop sharing the
    // team supervisor's stop token; runs go through the governed core executor.
    spawn_workflow_supervisor(
        workflow_rx,
        workflow_control.clone(),
        lifecycle.stop.clone(),
    );
    // Drive the event-sourced mailbox: append MailSent/Channel*/AgentRegistered to
    // the team-root log and serve roster/channel reads (ADR-0001).
    // Release idle Project scopes (and their plugin/bundle processes) even
    // when nothing binds.
    lifecycle.tasks.spawn(run_catalog_scope_sweeper(
        Arc::downgrade(&engine),
        lifecycle.stop.clone(),
        CATALOG_SCOPE_SWEEP_PERIOD,
    ));
    lifecycle.tasks.spawn(run_mailbox_service(
        engine.clone(),
        mailbox_rx,
        lifecycle.stop.clone(),
    ));
    lifecycle.tasks.spawn(run_lifecycle_service(
        engine.clone(),
        resident_supervisor.clone(),
        lifecycle_rx,
        lifecycle.stop.clone(),
    ));
    Ok(BuiltSessionEngine {
        engine,
        resident_supervisor,
        workflow_control,
        asks: Some(asks),
        questions: Some(questions),
        mcp_control,
        lifecycle,
        agent_model_control,
    })
}

/// How often idle catalog scopes are swept.
const CATALOG_SCOPE_SWEEP_PERIOD: std::time::Duration = std::time::Duration::from_secs(60);

/// Periodically evict idle and over-capacity catalog scopes until `stop`
/// fires or the engine is gone.
async fn run_catalog_scope_sweeper(
    engine: std::sync::Weak<SessionEngine>,
    stop: tokio_util::sync::CancellationToken,
    period: std::time::Duration,
) {
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick completes immediately; start sweeping one period in.
    ticks.tick().await;
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            _ = ticks.tick() => {}
        }
        let Some(engine) = engine.upgrade() else {
            break;
        };
        engine.sweep_catalog_scopes();
    }
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

/// Holds the built engine for the process lifetime so side
/// planes stay connected until the runtime is dropped.
pub struct HyaRuntime {
    router: axum::Router,
    engine: Arc<SessionEngine>,
    app_state: hya_server::AppState,
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
            let mut runtime = offline_runtime(opts.model, false);
            runtime.default_agent = opts.default_agent;
            runtime
        } else {
            let mut runtime = resolve_runtime(opts.model).await;
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
        // Environment + AGENTS + references once.
        let agent = Arc::new(agent_base_with_model(&runtime.model, None));
        let mut built = build_session_engine(
            store,
            runtime.router,
            agent.as_ref(),
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission, runtime.settings),
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
        let workflow_control = Arc::new(built.workflow_control());
        let agent_model_control = Arc::new(built.agent_model_control());
        let mut state = hya_server::AppState::new(engine.clone(), agent)
            .with_provider_control(Arc::new(crate::ProviderManager::new(engine.clone())))
            .with_bundle_control(Arc::new(crate::bundle_admin::BundleManager))
            .with_question_requests(questions)
            .with_mcp_control(mcp_control)
            .with_workflow_control(workflow_control)
            .with_agent_model_control(agent_model_control)
            .with_default_agent(runtime.default_agent.clone())
            .with_auto_title(true);
        state = state.with_permission_requests(asks);
        // Remembered "allow always" grants survive restarts: reload them into
        // the process permission plane before serving.
        if let Err(error) = state.restore_saved_permissions().await {
            tracing::warn!(%error, "failed to restore saved permission grants");
        }
        built.resume_interrupted(&state).await?;
        let app_state = state.clone();
        let router = hya_server::router(state);
        Ok(Self {
            router,
            engine,
            app_state,
            _built: built,
        })
    }

    /// Fence server admission, drain composition and residents, then join server services.
    pub async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.app_state.begin_shutdown();
        let result = self._built.shutdown().await;
        self.app_state.shutdown().await;
        result
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
mod tests;
