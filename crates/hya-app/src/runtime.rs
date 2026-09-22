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
    BoundWorkflowRequest, BoundWorkflowSender, CategoryRegistry, CompactionConfig, CoreError,
    EventBus, ModelSummarizer, PromptEnv, ResidentSupervisor, RuntimeRegistry, RuntimeSourceKind,
    SessionEngine, SidecarEnvironment, SidecarHandle, SidecarLifecycle, SidecarStart,
    SpawnAdmissionOutcome, SubagentGovernor, Summarizer, TokenAccounting, TurnBinding,
    apply_agent_model_preference, apply_spawn_model_policy, build_system_prompt,
    resolve_dispatch_model, run_lifecycle_service, run_mailbox_service,
};

// Single discovery/date implementation lives in hya-core; re-export for callers.
pub use hya_core::{discover_context_files, today};
use hya_mcp::McpServerConfig;
use hya_plugin::client::{ChildGuard, PluginClient};
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::{
    ActivationLifecycle, ActivationMetadata, HookName, HookRegistration, PluginKindWire, ToolInfo,
};
use hya_plugin::{HostInfo, PluginContributionSet};
use hya_proto::{
    AgentName, ModelRef, OwnerRunId, SessionId, SubagentMode, ToolName, ToolSchema,
    WorkflowDelivery,
};
use hya_provider::{DevProvider, ProviderCatalogSnapshot, ProviderRouter, ReasoningEffort};
use hya_store::{AdmissionTerminal, SessionStore, StoreError};
use hya_tool::{
    Action, AskRequest, InteractionPlane, InvocationPolicy, LifecyclePlane, MailboxPlane,
    MemberOutcome, Mode, PermissionModel, PermissionPlane, PermissionRules, QuestionRequest,
    ResolvedTool, Resource, Rule, SpawnError, SpawnMember, SpawnRequest, Tool, ToolCtx, ToolError,
    ToolPermission, ToolRegistry, WebSearchConfig, WebSearchPlane,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::OpenOptions;
use std::io::Write;

use crate::agent_model_control::PersistentAgentModelControl;
use crate::config;
use crate::runtime_reconcile::{
    DesiredSource, PreparedFailure, PreparedResult, RuntimeMcpControl, RuntimeReconciler, SourceId,
    adapt_prepared_bundle_skills, prepare_desired_source, prepared_plugin_source,
};
use crate::{InstalledBundleRefresh, bundle_registry_path, formatter_config, plugins};

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
        version: env!("CARGO_PKG_VERSION").to_string(),
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

/// Compaction thresholds from `config.yaml`, with `HYA_COMPACTION_*` env overrides.
///
/// Prefer [`crate::config::load_context_settings`] when the token-accounting
/// mode is needed too; this returns only the thresholds.
#[must_use]
pub fn compaction_config() -> CompactionConfig {
    crate::config::load_context_settings().compaction
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
}

impl BundleSidecarEnvironment {
    #[cfg(test)]
    fn from_command(command: Vec<String>, staging_root: PathBuf) -> Self {
        Self {
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
            .path_for(definition.origin)
            .and_then(|path| std::path::absolute(path).map_err(Into::into))
            .map_err(|error| {
                CoreError::Invalid(format!("resolve bundle configuration path: {error}"))
            })?;
        let config_dir = config_file.parent().ok_or_else(|| {
            CoreError::Invalid("bundle configuration path has no parent".to_string())
        })?;
        let environment = BTreeMap::from([
            (
                "HYA_BUNDLE_CONFIG_DIR".to_string(),
                config_dir.to_string_lossy().into_owned(),
            ),
            (
                "HYA_BUNDLE_CONFIG_FILE".to_string(),
                config_file.to_string_lossy().into_owned(),
            ),
        ]);
        let (client, mut guard) =
            match PluginClient::spawn_bundle(&command, activation_dir.path(), Some(&environment)) {
                Ok(result) => result,
                Err(error) => {
                    return Err(CoreError::Invalid(format!("spawn bundle sidecar: {error}")));
                }
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
            || initialized.plugin.kind != PluginKindWire::Bun
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
                &initialized.contributions.hooks,
            )?;
            validate_bundle_sidecar_skills(
                &self.binding,
                &self.stable_agent_id,
                &initialized.contributions,
            )?;
            let tools = bind_bundle_sidecar_tools(
                &self.binding,
                &self.stable_agent_id,
                &client,
                &initialized.contributions.tools,
            )?;
            let hooks = (!initialized.contributions.hooks.is_empty()).then(|| {
                Arc::new(hya_plugin::ActivationHookDispatcher::new(
                    client.clone(),
                    &initialized.contributions.hooks,
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
    let selected = policy
        .selected_bundle_skill_ids()
        .iter()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let resources = binding
        .bundle_catalog()
        .bundle_resources(bundle_id, hya_bundle::ExportKind::Skill)
        .ok_or_else(|| {
            CoreError::Invalid(format!(
                "bundle `{bundle_id}` has no prepared Skill namespace"
            ))
        })?
        .iter()
        .filter(|resource| selected.contains(resource.stable_id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    adapt_prepared_bundle_skills(bundle_id, &resources, contributions)
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

/// `--pure` variant of [`agent_with_model`]: same Environment block, but no
/// external AGENTS/context files are discovered or baked into the prompt.
pub fn agent_with_model_pure(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    let workdir = PathBuf::from(".");
    let env = PromptEnv {
        cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string()),
        platform: std::env::consts::OS.to_string(),
        date: today(),
    };
    let system_prompt = build_system_prompt(HARNESS_AGENT_BASE, &env, &[]);
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
    /// Live model routes (or the canonical offline provider).
    pub router: ProviderRouter,
    /// Shared immutable startup model/provider catalog.
    pub catalog: Arc<ProviderCatalogSnapshot>,
    /// Active request model id for new sessions.
    pub model: String,
    /// Default reasoning effort for the active model, when configured.
    pub reasoning: Option<ReasoningEffort>,
    /// MCP server configs to connect at engine build.
    pub mcp: BTreeMap<String, McpServerConfig>,
    /// Plugin specs already merged from config + manifests.
    pub plugins: Vec<PluginSpec>,
    /// Preferred primary agent when workdir does not select one.
    pub default_agent: Option<String>,
    /// Logical model categories the runtime resolves at subagent spawn time.
    pub categories: CategoryRegistry,
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
        reasoning: None,
        mcp: BTreeMap::new(),
        plugins: Vec::new(),
        default_agent: None,
        categories: CategoryRegistry::default(),
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
        pure: false,
    }
}

/// Resolve providers and the immutable catalog before returning runtime state.
pub async fn resolve_runtime(model_override: Option<String>) -> RuntimeConfig {
    match config::load().await {
        Ok(Some(cfg)) => {
            let model = model_override
                .or_else(|| std::env::var("HYA_MODEL").ok())
                .unwrap_or_else(|| cfg.default_model.clone());
            let reasoning = cfg
                .catalog
                .models()
                .iter()
                .find(|entry| {
                    entry.model_ref().as_str() == model
                        || (entry.model_id == model
                            && cfg
                                .catalog
                                .models()
                                .iter()
                                .filter(|candidate| candidate.model_id == model)
                                .count()
                                == 1)
                })
                .and_then(|entry| entry.reasoning_default);
            let offline_notice = cfg.catalog.notice().map(|_| OfflineNotice {
                config_path: config::expected_config_path(),
            });
            RuntimeConfig {
                router: cfg.router,
                catalog: cfg.catalog,
                model,
                reasoning,
                mcp: cfg.mcp,
                plugins: plugins::resolve(cfg.plugins, plugins::plugins_dir().as_deref()),
                default_agent: cfg.default_agent,
                categories: cfg.categories,
                offline_notice,
                permission: cfg.permission,
                websearch: cfg.websearch,
                pending_discovery: cfg.pending_discovery,
                pure: false,
            }
        }
        Ok(None) => offline_runtime(model_override, false),
        Err(error) => {
            eprintln!("hya: config error ({error:#}); using the offline provider");
            offline_runtime(model_override, true)
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
    members: &'a [SpawnMember],
}

fn spawn_request_fingerprint(req: &SpawnRequest) -> Result<[u8; 32], serde_json::Error> {
    let canonical = serde_json::to_vec(&SpawnRequestFingerprint {
        domain: "hya.spawn-admission.v1",
        parent: req.parent,
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
const AGENT_MODEL_PREFERENCE_IDENTITY_V1: &[u8] = b"AgentModelPreferenceResolutionV1";

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
    agent_model_preferences: Vec<u8>,
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
fn canonical_agent_model_preferences(
    preferences: &BTreeMap<String, ModelRef>,
) -> Result<Vec<u8>, AdmissionResolutionContextError> {
    let mut canonical = Vec::new();
    canonical.extend_from_slice(AGENT_MODEL_PREFERENCE_IDENTITY_V1);
    canonical.extend_from_slice(&canonical_count(preferences.len())?.to_be_bytes());
    for (agent_id, model) in preferences {
        append_len_prefixed(
            &mut canonical,
            agent_id.as_bytes(),
            canonical_length(agent_id.as_bytes())?,
        );
        append_len_prefixed(
            &mut canonical,
            model.as_str().as_bytes(),
            canonical_length(model.as_str().as_bytes())?,
        );
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
/// definition/preference chain.
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
    let resolved = resolve_dispatch_model(&requested, &model_ids, &provider_ids)
        .map(|model| model.to_string());
    member.model = resolved.clone();
    if let Some(inline) = member.inline_agent.as_mut() {
        inline.model = resolved;
    }
    member
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
    let mut agent =
        apply_spawn_model_policy(agent, definition, &member, ctx.categories, ctx.is_servable);
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
        let agent_model_preferences = canonical_agent_model_preferences(&BTreeMap::new())?;

        Ok(Self {
            base,
            categories,
            router,
            base_model_len,
            base_system_prompt_len,
            base_reasoning_len,
            category_resolution,
            provider_resolution,
            agent_model_preferences,
        })
    }

    fn with_agent_model_preferences(
        mut self,
        preferences: BTreeMap<String, ModelRef>,
    ) -> Result<Self, AdmissionResolutionContextError> {
        self.agent_model_preferences = canonical_agent_model_preferences(&preferences)?;
        Ok(self)
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
        canonical.extend_from_slice(&self.agent_model_preferences);
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

/// Return whether request-local routing fully suppresses an Agent default.
#[allow(dead_code)]
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
    workflow_control: crate::WorkflowControl,
    agent_model_control: PersistentAgentModelControl,
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

    fn active_run(&self, session: SessionId) -> Option<hya_proto::WorkflowRunId> {
        crate::WorkflowControl::active_run(self, session)
            .ok()
            .flatten()
    }

    fn cancel(&self, session: SessionId) -> bool {
        self.cancel_run(session)
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
    control: crate::WorkflowControl,
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
            let execution = handle_workflow_request(&control, binding, request);
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

/// Execute one framed workflow request through the app-owned control seam.
async fn handle_workflow_request(
    control: &crate::WorkflowControl,
    binding: TurnBinding,
    request: hya_tool::WorkflowRequest,
) {
    let hya_tool::WorkflowRequest {
        parent,
        operation,
        command,
        cancel,
        reply,
    } = request;
    let result = control
        .execute(
            parent,
            crate::WorkflowInvocation {
                caller: None,
                operation: Some(operation),
                binding: Some(binding),
                delivery: WorkflowDelivery::Finished,
            },
            command,
            cancel,
        )
        .await
        .map_err(|error| hya_tool::WorkflowHostError::new(error.code(), error.to_string()));
    let _ = reply.send(result);
}

fn spawn_team_supervisor_with_environment(
    mut rx: tokio::sync::mpsc::Receiver<BoundSpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    router: Arc<ProviderRouter>,
    categories: Arc<CategoryRegistry>,
    resident_supervisor: Arc<ResidentSupervisor>,
    sidecar_environment: Arc<BundleSidecarEnvironment>,
) -> SpawnSupervisorLifecycle {
    let stop = tokio_util::sync::CancellationToken::new();
    let stop_child = stop.child_token();
    let join = tokio::spawn(async move {
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
                    Ok(context) => Some(context),
                    Err(error) => {
                        let _ = req.reply.send(Err(error));
                        continue;
                    }
                }
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
            let _source_tool_call = req.operation.source_tool_call_id();
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
                let mut reply = Some(req.reply);
                let mut spawn_failed = false;

                // Unified path (ADR-0015): every member is a resident actor.
                // Register the team root as the main actor so child mail,
                // reports, and quiescence wake it.
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
                let mut outcomes = Vec::new();
                if let Err(err) = main_activation_result {
                    let summary = err.to_string();
                    eprintln!("hya: ensure_main failed ({summary})");
                    spawn_failed = true;
                    for _ in 0..resolved.len() {
                        outcomes.push(MemberOutcome {
                            member: "-".to_string(),
                            session: "-".to_string(),
                            status: "failed".to_string(),
                            summary: summary.clone(),
                        });
                    }
                } else {
                    for resolved in resolved {
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
                            Ok((session, handle)) => outcomes.push(MemberOutcome {
                                member: handle.clone(),
                                session: session.to_string(),
                                status: "running".to_string(),
                                summary: format!(
                                    "Resident {handle} is live; results arrive as its report."
                                ),
                            }),
                            Err(err) => {
                                spawn_failed = true;
                                outcomes.push(MemberOutcome {
                                    member: "-".to_string(),
                                    session: "-".to_string(),
                                    status: "failed".to_string(),
                                    summary: err.to_string(),
                                });
                            }
                        }
                    }
                }

                // The spawn operation completes at registration (ADR-0015 §2):
                // the admission journal covers spawn idempotency, not task
                // completion, which now travels as the child's report.
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
                    let _ = reply.take();
                    return;
                }
                if let Some(reply) = reply.take() {
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

/// Foreground budget for `mcp__` tool calls (`HYA_MCP_BACKGROUND_AFTER_MS`).
/// Unset, unparsable, or `0` keeps every MCP call synchronous; a positive
/// value moves calls still running past the budget to the background, where
/// completion is delivered as a steered reclaim prompt.
fn mcp_background_after_from_env() -> Option<std::time::Duration> {
    std::env::var("HYA_MCP_BACKGROUND_AFTER_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(std::time::Duration::from_millis)
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
    tool_config: (WebSearchConfig, InvocationPolicy),
    options: EngineBuildOptions,
) -> anyhow::Result<BuiltSessionEngine> {
    let owner_run_id = process_owner_run_id();
    store
        .claim_runtime_owner(owner_run_id)
        .context("claim runtime owner before startup recovery")?;
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
    let categories = Arc::new(crate::config::load_categories());
    let agent_model_control = PersistentAgentModelControl::load(
        store.clone(),
        owner_run_id,
        runtime.clone(),
        router.clone(),
    )
    .await
    .context("load Agent model preferences before engine readiness")?
    .with_categories(categories.clone())
    .with_configuration(crate::agent_model_config::AgentModelConfigFiles::new(
        crate::config::active_config_path(),
    ))
    .await
    .context("load Agent model configuration before engine readiness")?;
    let catalog_refresh = Arc::new(
        InstalledBundleRefresh::new(bundle_registry_path())
            .with_project_dir(crate::project_bundles::project_bundles_dir()),
    );

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
    let (lifecycle, lifecycle_rx) = LifecyclePlane::new();
    let summarizer: Arc<dyn Summarizer> =
        Arc::new(ModelSummarizer::new(router.clone(), agent.model.clone()));
    let bus = EventBus::new(crate::config::resolve_event_bus_capacity());
    let governor = SubagentGovernor::new(subagent_limits);
    // Clone the router before it is moved into the engine so the team supervisor
    // can test category-candidate servability against the same live providers.
    let spawn_router = router.clone();
    let sidecar_environment = Arc::new(BundleSidecarEnvironment::production());
    let context_settings = crate::config::load_context_settings();
    let mut engine_builder = SessionEngine::new(store, router, runtime, permission, bus)
        .with_catalog_refresh(catalog_refresh)
        .with_sidecar_environment(sidecar_environment.clone())
        .with_model_categories(categories.clone())
        // Route `categories:` failover chains into the engine's cross-model
        // plane so turn-time pre-stream failures advance through the ordered
        // candidates instead of failing the whole turn.
        .with_model_fallbacks(category_model_fallbacks(&categories))
        .with_compaction(summarizer, context_settings.compaction)
        .with_token_accounting(TokenAccounting::new(context_settings.token_accounting))
        .with_formatter(formatter_config::load_plane())
        .with_lsp(crate::lsp::load_plane()?)
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
        .with_lifecycle(lifecycle)
        .with_governor(governor);
    if let Some(budget) = mcp_background_after_from_env() {
        engine_builder = engine_builder.with_mcp_background_after(budget);
    }
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
        let recovered_binding = engine
            .bind_session_runtime(actor_id, &workdir)
            .await
            .context("bind recovered resident Session model configuration")?;
        let (recovered_binding, recovered_agent) =
            resolve_recovered_resident_agent(&engine, recovered_binding, agent, recorded)
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
    let workflow_control = crate::WorkflowControl::new_with_routing(
        engine.clone(),
        agent.clone(),
        resident_supervisor.clone(),
        owner_run_id,
        categories.clone(),
        spawn_router.clone(),
    );
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
        workflow_control.clone(),
        lifecycle.stop.clone(),
    ));
    // Drive the event-sourced mailbox: append MailSent/Channel*/AgentRegistered to
    // the team-root log and serve roster/channel reads (ADR-0001).
    tokio::spawn(run_mailbox_service(engine.clone(), mailbox_rx));
    tokio::spawn(run_lifecycle_service(
        engine.clone(),
        resident_supervisor.clone(),
        lifecycle_rx,
    ));
    Ok(BuiltSessionEngine {
        engine,
        resident_supervisor,
        workflow_control,
        asks: Some(asks),
        questions: Some(questions),
        mcp_control,
        plugin_host,
        lifecycle,
        agent_model_control,
    })
}

/// Exact-resolve a process-loss recovered resident from the current RuntimeSnapshot.
///
/// Binds once from the recorded session workdir and uses the same production
/// TurnBinding catalog projection as live turns. Resume is definition resolution,
/// not a new spawn: no `can_spawn`, no AgentSpec synthesis, no general/base fallback.
fn resolve_recovered_resident_agent(
    engine: &SessionEngine,
    binding: TurnBinding,
    base: &AgentSpec,
    recorded_agent: &AgentName,
) -> Result<(TurnBinding, AgentSpec), CoreError> {
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
                match prepared_plugin_source(plugin.clone()) {
                    Ok(prepared) => PreparedResult::from(prepared),
                    Err(error) => PreparedResult::from(PreparedFailure::new(
                        source.id().clone(),
                        format!("PLUGIN_CONTRIBUTION_INVALID: {error}"),
                    )),
                }
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
        let workflow_control = Arc::new(built.workflow_control());
        let agent_model_control = Arc::new(built.agent_model_control());
        let plugin_host = built.plugin_host();
        let mut state = hya_server::AppState::new(engine.clone(), agent)
            .with_question_requests(questions)
            .with_mcp_control(mcp_control)
            .with_workflow_control(workflow_control)
            .with_agent_model_control(agent_model_control)
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

    use hya_bundle::SpawnLifecycle;
    use hya_core::CreateSession;
    use hya_core::{MemberSpec, MemberStatus};
    use hya_proto::MemberId;

    /// Test shim: wrap an installed-bundle catalog as an [`AgentCatalog`] over
    /// the compiled-in built-ins.
    fn to_agent_catalog(bundles: BundleCatalog) -> AgentCatalog {
        AgentCatalog::new(Arc::new(bundles)).expect("valid agent catalog")
    }

    /// Wrap one prepared singular AgentBundle for a closed catalog payload.
    fn agent_bundle(bundle: PreparedAgentBundle) -> PreparedInstallableBundle {
        PreparedInstallableBundle::Agent(Box::new(bundle))
    }

    /// Wrap prepared singular AgentBundles for a closed catalog payload.
    fn agent_bundles(
        bundles: impl IntoIterator<Item = PreparedAgentBundle>,
    ) -> Vec<PreparedInstallableBundle> {
        bundles.into_iter().map(agent_bundle).collect()
    }

    use super::*;
    use async_trait::async_trait;
    use hya_bundle::{
        AgentRole, BundleIdentity, BundleSource, ModelPolicy, PreparedAgent, PreparedAgentBundle,
        PreparedInstallableBundle, PreparedResource, ResourceView, SourceFile, prepare_package,
    };
    use hya_core::{CategoryEntry, run_team};
    use hya_plugin::messages::{METHOD_TOOL_CALL, ToolCallParams, ToolInfo};
    use hya_plugin::protocol::Frame;
    use hya_proto::{
        Event, FinishReason, MailEndpoint, MailKind, MemberRunStatus, OwnerRunId, RosterStatus,
        SubagentMode, ToolName, ToolSchema, WorkflowIdentity, WorkflowRevision, WorkflowStagePlan,
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
        WebSearchPlane, handle::ArtifactPlane,
    };
    use serde_json::{Value, json};
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
            if model.as_str() == "dev" {
                return self.inner.capabilities(&ModelRef::new("hya/offline"));
            }
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
    fn resident_owner_run_id_is_stable_for_the_process() {
        assert_eq!(process_owner_run_id(), process_owner_run_id());
    }

    #[test]
    fn builtin_agent_catalog_includes_first_party_workflow_bundle() {
        let catalog = builtin_agent_catalog().expect("builtin agent catalog must build");
        assert!(
            catalog.bundles().bundles().len() >= 2,
            "fresh process includes the immutable first-party bundles"
        );
        assert!(
            catalog
                .bundles()
                .resolve_workflow("plan-impl-review")
                .is_some(),
            "first-party Workflow must be cataloged"
        );
        let goal_loop = catalog
            .bundles()
            .bundles()
            .iter()
            .find(|bundle| bundle.identity().id == "hya/goal-loop")
            .expect("goal-loop first-party bundle");
        assert_eq!(
            goal_loop.kind(),
            hya_bundle::PreparedBundleKind::AgentSetBundle
        );
        assert_eq!(goal_loop.agents().len(), 2);
        for skill in [
            "goal-contract",
            "guided-goal",
            "evaluator-prompt",
            "loop-verifier-prompt",
            "loop-planner-prompt",
        ] {
            assert!(
                goal_loop
                    .skills()
                    .iter()
                    .any(|resource| resource.local_id == skill)
            );
        }
        for id in ["build", "plan", "explore", "general", "hya-main"] {
            assert!(
                catalog.resolve(id).is_some(),
                "builtin `{id}` must resolve without any installed bundle"
            );
        }
    }

    #[test]
    fn builtin_agent_catalog_retains_semantic_identity_with_first_party() {
        let catalog = builtin_agent_catalog().expect("builtin agent catalog must build");
        assert!(
            catalog
                .semantic_identity_v1()
                .is_some_and(|identity| !identity.is_empty()),
            "first-party catalog must yield a semantic identity"
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

    #[test]
    fn admission_binding_fingerprint_includes_canonical_relevant_preferences() {
        let make_context = || {
            AdmissionResolutionContext::capture(
                AgentSpec {
                    name: AgentName::new("build"),
                    model: ModelRef::new("fixture/model"),
                    system_prompt: "captured harness base".to_string(),
                    workdir: PathBuf::from("fixture-workdir"),
                    reasoning: None,
                },
                Arc::new(CategoryRegistry::default()),
                Arc::new(ProviderRouter::new()),
            )
            .expect("empty provider context must capture")
        };
        let runtime_fingerprint = [0x5a; 32];
        let first = make_context()
            .with_agent_model_preferences(BTreeMap::from([
                ("reviewer".to_string(), ModelRef::new("provider/reviewer")),
                ("worker".to_string(), ModelRef::new("provider/first")),
            ]))
            .expect("preferences must canonicalize")
            .admission_binding_fingerprint_v1(runtime_fingerprint);
        let reordered = make_context()
            .with_agent_model_preferences(BTreeMap::from([
                ("worker".to_string(), ModelRef::new("provider/first")),
                ("reviewer".to_string(), ModelRef::new("provider/reviewer")),
            ]))
            .expect("preference order must not matter")
            .admission_binding_fingerprint_v1(runtime_fingerprint);
        let changed = make_context()
            .with_agent_model_preferences(BTreeMap::from([
                ("reviewer".to_string(), ModelRef::new("provider/reviewer")),
                ("worker".to_string(), ModelRef::new("provider/second")),
            ]))
            .expect("changed preference must canonicalize")
            .admission_binding_fingerprint_v1(runtime_fingerprint);

        assert_eq!(first, reordered);
        assert_ne!(first, changed);
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
            Some("credential-a".to_string()),
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
            Some("credential-b".to_string()),
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
            Some("credential-a".to_string()),
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
            Some("credential-a".to_string()),
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
                Some("credential".to_string()),
                ["primary".to_string()],
            )
            .expect("route-a HTTP provider must construct")
        };
        let route_b = || {
            HttpProvider::new(
                "route-b",
                ProviderKind::OpenAiCompatible,
                "https://route.example/shared",
                Some("credential".to_string()),
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

    /// Building a new runtime owner terminalizes persisted Workflow work before
    /// any supervisor can admit a Stage.
    #[tokio::test]
    async fn build_session_engine_interrupts_prior_workflow_owner() {
        let store = SessionStore::connect_memory().await.unwrap();
        let session = SessionId::new();
        let run = hya_proto::WorkflowRunId::new();
        let prior_owner = OwnerRunId::new();
        assert_ne!(prior_owner, process_owner_run_id());
        store
            .append_event(
                session,
                &Event::WorkflowRunStarted {
                    session,
                    run,
                    workflow: WorkflowIdentity {
                        source: hya_proto::WorkflowSourceId::new("test:restart-flow"),
                        name: "restart-flow".to_string(),
                        revision: WorkflowRevision::from_bytes([1; 32]),
                    },
                    request_hash: "inputs".to_string(),
                    owner: prior_owner,
                    stages: vec![WorkflowStagePlan {
                        id: "stage".to_string(),
                        title: None,
                        agent: hya_proto::AgentName::new("general"),
                        mode: "once".to_string(),
                        level: 0,
                        worker_model: None,
                        selected_worker_model: None,
                        verifier_model: None,
                        selected_verifier_model: None,
                    }],
                },
            )
            .await
            .unwrap();
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);

        let mut built = build_session_engine(
            store,
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();

        let current = built
            .engine()
            .read_projection(session)
            .await
            .unwrap()
            .session
            .workflow
            .unwrap()
            .run
            .unwrap();
        assert_eq!(current.status, hya_proto::WorkflowRunStatus::Interrupted);
        assert_eq!(current.error.as_deref(), Some("backend startup recovery"));
        built.shutdown().await.unwrap();
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
                Arc::clone(&router),
                runtime,
                permission.clone(),
                EventBus::default(),
            )
            .with_workflow_sender(workflow_sender.clone()),
        );
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("hya/offline"),
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
        let owner = process_owner_run_id();
        let resident_supervisor = ResidentSupervisor::start_with_owner(Arc::clone(&engine), owner);
        let control = crate::WorkflowControl::new_with_routing(
            Arc::clone(&engine),
            base,
            resident_supervisor,
            owner,
            Arc::new(CategoryRegistry::default()),
            router,
        );
        let supervisor = spawn_workflow_supervisor(workflow_rx, control, stop.clone());
        let (interaction, _interaction_rx) = InteractionPlane::new();
        let (spawner, _spawner_rx) = SpawnerPlane::new();
        let ctx = ToolCtx {
            workflows,
            permission: permission.for_session(lead),
            interaction: interaction.for_session(lead),
            spawner,
            operation: ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
            mailbox: MailboxPlane::disconnected(),
            lifecycle: LifecyclePlane::disconnected(),
            session: Some(lead),
            parent_session: None,
            todo: TodoPlane::default(),
            skills: SkillPlane::default(),
            artifacts: ArtifactPlane::default(),
            websearch: WebSearchPlane::default(),
            lsp: LspPlane::default(),
            formatter: FormatterPlane::default(),
            agents: Default::default(),
            workdir,
            cancel: CancellationToken::new(),
        };
        let run = tokio::spawn(async move {
            let workflow = hya_tool::ToolRegistry::builtins()
                .get("workflow")
                .expect("workflow tool");
            workflow
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
        assert_eq!(output["kind"], "run");
        assert_eq!(output["result"]["run"]["status"], "cancelled");
    }

    struct RuntimeMarker(&'static str);

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

    fn temp_socket(tag: &str) -> PathBuf {
        static NEXT_SOCKET_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let serial = NEXT_SOCKET_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // A tempdir() path already fills most of macOS's 104-byte sun_path
        // budget, so unix sockets bind directly in the temp root under a
        // short unique name.
        std::env::temp_dir().join(format!("hya-sock-{tag}-{nanos}-{serial}.sock"))
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

    #[tokio::test]
    async fn resolve_runtime_without_config_carries_but_does_not_print_the_notice() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        let _ = std::fs::remove_file(&config_path);

        let runtime = resolve_runtime(None).await;

        // Offline fallback selected: the canonical built-in hya/offline model.
        assert_eq!(runtime.model, "hya/offline");
        // The guidance is returned as DATA — resolve_runtime itself prints
        // nothing, so headless/RPC/serve callers (which never call `emit`) keep
        // a clean machine-readable stdout. Only interactive startup emits it.
        let notice = runtime
            .offline_notice
            .expect("missing-config path must carry an offline notice");
        assert!(notice.config_path.ends_with("hya/config.yaml"));
        assert!(notice.render().contains("OFFLINE"));
    }

    #[tokio::test]
    async fn permission_only_config_is_kept_and_config_errors_fall_back_to_strict() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "permission:\n  model: allow\n").unwrap();

        let runtime = resolve_runtime(None).await;
        assert_eq!(runtime.model, "hya/offline");
        assert_eq!(runtime.permission.model(), PermissionModel::Allow);
        assert!(runtime.offline_notice.is_some());
        assert_eq!(
            runtime.with_yolo(true).permission.model(),
            PermissionModel::Danger
        );

        std::fs::write(
            &config_path,
            "permission:\n  rules:\n    - target: tool\n      selector: '('\n      permission: Allow\n",
        )
        .unwrap();
        let fallback = resolve_runtime(None).await;
        assert_eq!(fallback.permission.model(), PermissionModel::Strict);
    }

    #[tokio::test]
    async fn websearch_only_config_reaches_runtime() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "tools:\n  websearch:\n    provider: parallel\n    endpoint: https://search.example.test/mcp\n    key: secret\n    enabled: false\n",
        )
        .unwrap();

        let runtime = resolve_runtime(None).await;

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
        assert!(runtime.offline_notice.is_some());
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
                hya_store::NamespaceInstallPolicy::DenyConflicts,
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
                hya_store::NamespaceInstallPolicy::DenyConflicts,
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
            EngineBuildOptions {
                defer_mcp: true,
                pure: false,
            },
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
            plugin_dir: None,
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
            EngineBuildOptions {
                defer_mcp: false,
                pure: false,
            },
        )
        .await;
        let mut built = result.unwrap();
        let engine = built.engine();
        let _ = built.take_asks();
        let _ = built.take_questions();
        let _built = built;
        let manifest = engine.runtime_registry().effective_manifest();
        // +1 for the goal-loop first-party bundle's skills, +1 for the
        // eager MCP server + plugin (published together in one revision).
        assert_eq!(
            manifest.generation.get(),
            hya_proto::ConfigGeneration::INITIAL.get() + 2
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
        assert!(names.contains(&"mixed-plugin__plugin_ping".to_string()));
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

    #[tokio::test]
    async fn selected_model_reasoning_default_reaches_first_agent() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "default_model: gateway/gpt-5.6-sol\nproviders:\n  gateway:\n    kind: openai-response\n    base_url: https://example.test/v1\n    api_key: test\n    models:\n      - id: gpt-5.6-sol\n        reasoning:\n          default: medium\n          variants: [low, medium]\n",
        )
        .unwrap();

        let runtime = resolve_runtime(None).await;

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
    fn agent_with_model_pure_keeps_environment_but_drops_agents_context() {
        let home = tempdir();
        let workdir = tempdir();
        let _env = EnvGuard::set(&home, &workdir);
        let agents_marker = "PURE_MODE_MUST_NOT_SEE_THIS";
        std::fs::write(workdir.join("AGENTS.md"), agents_marker).unwrap();

        let agent = agent_with_model_pure("fake", None);

        assert!(
            agent.system_prompt.contains(HARNESS_AGENT_BASE),
            "pure agent must keep the harness base: {}",
            agent.system_prompt
        );
        assert!(
            agent.system_prompt.contains("## Environment"),
            "pure agent must keep Environment: {}",
            agent.system_prompt
        );
        assert!(
            !agent.system_prompt.contains(agents_marker),
            "pure agent must not bake process-cwd AGENTS: {}",
            agent.system_prompt
        );
    }

    #[test]
    fn with_pure_clears_mcp_marks_runtime_and_keeps_websearch() {
        let mut runtime = offline_runtime(None, false);
        runtime.mcp.insert(
            "external".to_string(),
            hya_mcp::McpServerConfig {
                command: vec!["echo".to_string()],
                env: None,
                url: None,
                transport: None,
                enabled: None,
                timeout_ms: None,
            },
        );
        let websearch = runtime.websearch.clone();

        let runtime = runtime.with_pure(true);

        assert!(runtime.pure);
        assert!(runtime.mcp.is_empty(), "pure clears configured MCP servers");
        assert!(runtime.plugins.is_empty(), "pure clears configured plugins");
        assert_eq!(
            runtime.websearch.endpoint, websearch.endpoint,
            "websearch keeps its own configuration in pure mode"
        );
    }

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
            .map(|stable_id| {
                PreparedInstallableBundle::Agent(Box::new(PreparedAgentBundle {
                    format_version: 2,
                    identity: BundleIdentity {
                        id: format!("hya/recovery-resolution-{stable_id}"),
                        version: "0.0.0".to_string(),
                        publisher: "hya-tests".to_string(),
                    },
                    namespace: None,
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
                }))
            })
            .collect::<Vec<_>>();
        Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&bundles).expect("valid recovery catalog"),
        ))
    }

    #[tokio::test]
    async fn recovered_resident_missing_definition_fails_closed_without_synthesis() {
        let workdir = tempdir();
        let engine = engine_with_catalog(catalog_with_agents(&[])).await;
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base-model"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let recorded = AgentName::new("legacy-resident-only");

        let err = match resolve_recovered_resident_agent(
            &engine,
            engine.bind_runtime(&workdir).unwrap(),
            &base,
            &recorded,
        ) {
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
        let engine = engine_with_catalog(catalog_with_agents(&["resident-helper"])).await;
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base-model"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let recorded = AgentName::new("resident-helper");

        let (_binding, resolved) = resolve_recovered_resident_agent(
            &engine,
            engine.bind_runtime(&workdir).unwrap(),
            &base,
            &recorded,
        )
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
        let engine = engine_with_catalog(catalog_with_agents(&["resident-helper"])).await;
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base-model"),
            system_prompt: "lead base".to_string(),
            workdir: base_dir.clone(),
            reasoning: None,
        };
        let recorded = AgentName::new("resident-helper");

        let (_binding, resolved) = resolve_recovered_resident_agent(
            &engine,
            engine.bind_runtime(&session_dir).unwrap(),
            &base,
            &recorded,
        )
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
            PreparedInstallableBundle::Agent(Box::new(PreparedAgentBundle {
                format_version: 2,
                identity: BundleIdentity {
                    id: format!("hya/spawn-model-precedence-{stable_id}"),
                    version: "0.0.0".to_string(),
                    publisher: "hya-tests".to_string(),
                },
                namespace: None,
                digest: format!("test-only-{stable_id}"),
                agent,
                tools: Vec::new(),
                skills: Vec::new(),
                mcp: Vec::new(),
                hooks: Vec::new(),
                extensions: Vec::new(),
            }))
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

    #[tokio::test]
    async fn unconfigured_spawn_uses_remembered_model_below_explicit_override() {
        let workdir = tempdir();
        let engine = engine_with_catalog(catalog_with_worker_policy(ModelPolicy::default())).await;
        engine.runtime_registry().publish_agent_model_preferences(
            [("worker".to_string(), ModelRef::new("remembered/model"))]
                .into_iter()
                .collect(),
        );
        let binding = engine
            .bind_runtime(&workdir)
            .expect("bind preference snapshot");
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
        let sidecar_environment = BundleSidecarEnvironment::from_command(
            vec!["bun".to_string(), "sidecar.js".to_string()],
            tempdir(),
        );
        let resolve = |model: Option<&str>| {
            resolve_spawn_member(
                &ResolveSpawnMemberCtx {
                    engine: &engine,
                    binding: &binding,
                    base: &base,
                    caller: "build",
                    allowed_agents: &allowed,
                    categories: &categories,
                    is_servable: &is_servable,
                    guidance: None,
                    sidecar_environment: &sidecar_environment,
                },
                SpawnMember {
                    description: "remembered preference".to_string(),
                    prompt: "resolve model only".to_string(),
                    subagent_type: "worker".to_string(),
                    model: model.map(str::to_string),
                    ..SpawnMember::default()
                },
            )
            .expect("authorized worker must resolve")
            .agent
            .model
        };

        assert_eq!(resolve(None), ModelRef::new("remembered/model"));
        // Dispatch-time resolution: an explicit id that is not in the catalog
        // and substring-matches nothing defers to the remembered tier instead
        // of overriding verbatim. (Exact and substring dispatch are covered by
        // `resolve_dispatch_model` unit tests and the p22 e2e scenario.)
        assert_eq!(
            resolve(Some("explicit/model")),
            ModelRef::new("remembered/model")
        );
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
                label: "dispatchable inline model over Bundle model",
                bundle_model: Some("bundle/model"),
                bundle_category: Some("bundle-cat"),
                inline_model: Some("hya/offline"),
                inline_category: Some("inline-cat"),
                spawn_model: None,
                spawn_category: None,
                expected: "hya/offline",
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
                label: "dispatchable spawn model highest",
                bundle_model: Some("bundle/model"),
                bundle_category: Some("bundle-cat"),
                inline_model: Some("inline/model"),
                inline_category: Some("inline-cat"),
                spawn_model: Some("offline"),
                spawn_category: Some("spawn-cat"),
                expected: "hya/offline",
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
            BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("executable")]))
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
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "bun"},
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
            BundleCatalog::from_prepared(&agent_bundles([bundle]))
                .expect("activation hook fixture catalog"),
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
            BundleCatalog::from_prepared(&agent_bundles([disjoint_materialized_bundle(
                "selected-hooks",
            )]))
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

    /// Sidecars must declare exactly the prepared Skills selected by their bound agent.
    #[test]
    fn bundle_sidecar_skill_declarations_match_captured_selected_set() {
        let make_skill = |local_id: &str| {
            let content = format!(
                "---\nname: {local_id}\ndescription: {local_id} fixture\n---\n{local_id} body\n"
            );
            PreparedResource {
                local_id: local_id.to_string(),
                stable_id: format!("bundle:hya/materialized/skill/{local_id}"),
                source_path: format!("resources/skills/{local_id}.md"),
                digest: format!("{:x}", Sha256::digest(content.as_bytes())),
                content,
                binary_base64: None,
                aliases: Vec::new(),
            }
        };
        let selected = make_skill("selected");
        let unselected = make_skill("unselected");
        let mut bundle = disjoint_materialized_bundle("selected-skills");
        bundle
            .agent
            .resource_view
            .allow
            .push(selected.stable_id.clone());
        bundle.skills = vec![selected.clone(), unselected.clone()];

        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&agent_bundles([bundle]))
                .expect("selected-skills fixture catalog"),
        ));
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let full_contributions = PluginContributionSet {
            skills: [&selected, &unselected]
                .into_iter()
                .map(|resource| hya_plugin::SkillContribution {
                    id: resource.local_id.clone(),
                    content: resource.content.clone(),
                    digest: resource.digest.clone(),
                })
                .collect(),
            ..PluginContributionSet::default()
        };
        let source = crate::runtime_reconcile::prepared_static_bundle_source(
            "hya/materialized",
            &[selected.clone(), unselected.clone()],
            &full_contributions,
        )
        .expect("prepare selected-skills runtime source")
        .into_runtime_source();
        registry
            .refresh(|candidate| candidate.upsert_sources(vec![source]))
            .expect("publish selected-skills runtime source");
        let turn_dir = tempdir();
        let binding = registry
            .bind_turn(&turn_dir)
            .expect("capture selected-skills turn binding");

        let selected_contributions = PluginContributionSet {
            skills: vec![hya_plugin::SkillContribution {
                id: selected.local_id.clone(),
                content: selected.content.clone(),
                digest: selected.digest.clone(),
            }],
            ..PluginContributionSet::default()
        };
        assert!(validate_bundle_sidecar_skills(&binding, "alpha", &selected_contributions).is_ok());
        assert!(
            validate_bundle_sidecar_skills(&binding, "alpha", &full_contributions).is_err(),
            "an unselected prepared Skill declaration must fail closed"
        );
        assert!(
            validate_bundle_sidecar_skills(&binding, "alpha", &PluginContributionSet::default(),)
                .is_err(),
            "a missing selected Skill declaration must fail closed"
        );
    }

    #[tokio::test]
    async fn bundle_sidecar_handle_shutdown_reaps_child_and_removes_activation_staging() {
        let catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&agent_bundles([materialized_tool_bundle("shutdown")]))
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
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "bun"},
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
            BundleCatalog::from_prepared(&agent_bundles([materialized_tool_bundle(
                "drop-cleanup",
            )]))
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
            BundleCatalog::from_prepared(&agent_bundles([materialized_tool_bundle(
                "cancel-start",
            )]))
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
            BundleCatalog::from_prepared(&agent_bundles([materialized_tool_bundle("loss-token")]))
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
        assert_eq!(prepared.bundles()[0].extensions().len(), 1);
        assert_eq!(
            prepared.bundles()[0].extensions()[0].source_path,
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
            BundleCatalog::from_prepared(&agent_bundles([bundle]))
                .expect("Bun extension fixture catalog"),
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
            BundleCatalog::from_prepared(&agent_bundles(bundles))
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
            BundleCatalog::from_prepared(&agent_bundles([bundle]))
                .expect("generic Bun superset fixture catalog"),
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
            BundleCatalog::from_prepared(&agent_bundles([materialized_bun_bundle("bun-e2e")]))
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
            BundleCatalog::from_prepared(&agent_bundles([bundle]))
                .expect("root Bun hook fixture catalog"),
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
            BundleCatalog::from_prepared(&agent_bundles([bundle]))
                .expect("resident Bun fixture catalog"),
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
                ..SpawnMember::default()
            },
        )
        .expect("resolve resident Bun spawn member");
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
            BundleCatalog::from_prepared(&agent_bundles([bundle]))
                .expect("resident loss fixture catalog"),
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
        let socket_path = temp_socket("sl");
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
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "bun"},
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
            BundleCatalog::from_prepared(&agent_bundles([materialized_bundle(
                "resident-replacement",
            )]))
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
            BundleCatalog::from_prepared(&agent_bundles([bundle]))
                .expect("resident running loss fixture catalog"),
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
        let marker_socket = temp_socket("rrlm");
        let incarnation_claim = staging_root.join("resident-running-loss-incarnation");
        let release_socket = temp_socket("rrlr");
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
            "plugin": {"id": "bundle-sidecar", "version": "0.1.0", "kind": "bun"},
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
            BundleCatalog::from_prepared(&agent_bundles([materialized_bundle(
                "resident-running-replacement",
            )]))
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
            BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("unsupported-hook")]))
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
            binary_base64: None,
            aliases: Vec::new(),
        }
    }

    fn materialized_bundle(marker: &str) -> PreparedAgentBundle {
        let mut resource_view = ResourceView::default();
        resource_view.allow.push("echo".to_string());
        PreparedAgentBundle {
            format_version: 2,
            identity: BundleIdentity {
                id: "hya/materialized".to_string(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            namespace: None,
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

    fn materialized_tool_bundle(marker: &str) -> PreparedAgentBundle {
        let mut bundle = materialized_bundle(marker);
        bundle.hooks.clear();
        bundle.agent.hook_refs.clear();
        bundle
    }

    /// Two bundles with disjoint closures: `alpha` and `beta`, each owning its
    /// own tool, hook, and extension.
    fn disjoint_materialized_bundles(marker: &str) -> Vec<PreparedAgentBundle> {
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

    fn disjoint_materialized_bundle(marker: &str) -> PreparedAgentBundle {
        let alpha_path = "extensions/alpha.js";
        let beta_path = "extensions/beta.js";
        let alpha_tool = materialized_resource(marker, "tool", "echo", alpha_path);
        let alpha_hook = materialized_resource(marker, "hook", "event", alpha_path);
        let alpha_extension = materialized_resource(marker, "extension", "alpha", alpha_path);
        let beta_tool = materialized_resource(marker, "tool", "beta", beta_path);
        let beta_hook = materialized_resource(marker, "hook", "tool.execute.before", beta_path);
        let beta_extension = materialized_resource(marker, "extension", "beta", beta_path);
        PreparedAgentBundle {
            format_version: 2,
            identity: BundleIdentity {
                id: "hya/materialized".to_string(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            namespace: None,
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
            BundleCatalog::from_prepared(&agent_bundles([selector, owner]))
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

    fn set_materialized_extension_content(bundle: &mut PreparedAgentBundle, content: String) {
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
        bundle: &mut PreparedAgentBundle,
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

    fn materialized_bun_bundle(marker: &str) -> PreparedAgentBundle {
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
            BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("old")]))
                .expect("old fixture catalog"),
        ));
        let new_catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("new")]))
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
            BundleCatalog::from_prepared(&agent_bundles(disjoint_materialized_bundles("old")))
                .expect("old disjoint fixture catalog"),
        ));
        let new_catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&agent_bundles(disjoint_materialized_bundles("new")))
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
            BundleCatalog::from_prepared(&agent_bundles([bundle]))
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
            BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("old")]))
                .expect("old fixture catalog"),
        ));
        let mut new_bundle = materialized_bundle("new");
        new_bundle.tools.clear();
        let new_catalog = Arc::new(to_agent_catalog(
            BundleCatalog::from_prepared(&agent_bundles([new_bundle]))
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
            BundleCatalog::from_prepared(&agent_bundles([disjoint_materialized_bundle(
                "selected-tools",
            )]))
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
            BundleCatalog::from_prepared(&agent_bundles([bundle]))
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
            BundleCatalog::from_prepared(&agent_bundles([bundle]))
                .expect("hook-only fixture catalog"),
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
            lifecycle: LifecyclePlane::disconnected(),
            session: Some(session),
            parent_session: None,
            todo: TodoPlane::default(),
            skills: SkillPlane::default(),
            artifacts: ArtifactPlane::default(),
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
            BundleCatalog::from_prepared(&agent_bundles([lead_bundle, worker_bundle]))
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
            BundleCatalog::from_prepared(&agent_bundles([materialized_bundle("executable")]))
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
            BundleCatalog::from_prepared(&agent_bundles([static_bundle]))
                .expect("static fixture catalog"),
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
}
