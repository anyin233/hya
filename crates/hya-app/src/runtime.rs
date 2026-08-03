// allow: SIZE_OK — reviewed Phase 1 keeps backend bootstrap glue in this public API module.
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use hya_bundle::{BundleCatalog, PreparedAgent, PreparedCatalog, SpawnLifecycle};
use hya_core::{
    AgentResourcePolicy, AgentSpec, BoundSidecarFactory, BoundSpawnRequest, BoundSpawnSender,
    CategoryRegistry, CompactionConfig, CoreError, CreateSession, EventBus, MemberSpec,
    MemberStatus, ModelSummarizer, PromptEnv, ResidentSupervisor, RuntimeRegistry, SessionEngine,
    SidecarEnvironment, SidecarHandle, SidecarLifecycle, SidecarStart, SpawnAdmissionOutcome,
    SubagentGovernor, Summarizer, TeamEvidenceEnvelope, TurnBinding, build_system_prompt,
    project_envelope, project_envelope_for_actor, run_mailbox_service, run_pre_admitted_team,
    run_pre_admitted_team_for_actor,
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
use hya_store::{AdmissionIntent, AdmissionTerminal, SessionStore, StoreError};
use hya_tool::{
    Action, AskRequest, InteractionPlane, InvocationPolicy, MailboxPlane, MemberOutcome, Mode,
    PermissionModel, PermissionPlane, PermissionRules, QuestionRequest, ResolvedTool, Resource,
    Rule, SpawnError, SpawnMember, SpawnRequest, Tool, ToolCtx, ToolError, ToolPermission,
    ToolRegistry, WebSearchConfig, WebSearchPlane,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;

use crate::config;
use crate::runtime_reconcile::{
    DesiredSource, PreparedFailure, PreparedResult, RuntimeMcpControl, RuntimeReconciler, SourceId,
    prepare_desired_source, prepared_plugin_source,
};
use crate::spawn_intent::{PriorStartV1, SpawnIntentInputV1, SpawnIntentV1};
use crate::{InstalledBundleRefresh, bundle_registry_path, formatter_config, plugins};

const BUILTIN_BUNDLES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/builtin-bundles.json"));
const BUILTIN_BUNDLES_DIGEST: &str =
    include_str!(concat!(env!("OUT_DIR"), "/builtin-bundles.sha256"));

/// Injectable decode path for invalid/tamper unit tests. Production bootstrap
/// uses [`builtin_catalog`], which caches the embedded artifact once.
#[cfg(test)]
fn builtin_catalog_from(bytes: &[u8], expected_digest: &str) -> anyhow::Result<Arc<BundleCatalog>> {
    let prepared = PreparedCatalog::decode(bytes, expected_digest)
        .context("decode embedded built-in AgentBundle catalog")?;
    let catalog = BundleCatalog::from_verified_catalogs(&[&prepared])
        .context("validate embedded built-in AgentBundle catalog")?;
    Ok(Arc::new(catalog))
}

/// Decode and validate the build-embedded prepared catalog exactly once.
///
/// Success and failure are both cached so a corrupt embedded artifact stays
/// fail-closed for the process lifetime without silent retry or substitution.
/// Read-only public accessor for consumers (e.g. `hya-backend` agent list) that
/// must share the same process-wide `Arc` — no second embed, decode, or cache.
/// Injectable [`builtin_catalog_from`] remains uncached for tamper tests.
pub fn builtin_catalog() -> anyhow::Result<Arc<BundleCatalog>> {
    use hya_bundle::BundleError;

    static EMBEDDED_CATALOG: OnceLock<Result<Arc<BundleCatalog>, BundleError>> = OnceLock::new();
    match EMBEDDED_CATALOG.get_or_init(|| {
        PreparedCatalog::decode(BUILTIN_BUNDLES, BUILTIN_BUNDLES_DIGEST)
            .and_then(|prepared| BundleCatalog::from_verified_catalogs(&[&prepared]))
            .map(Arc::new)
    }) {
        Ok(catalog) => Ok(Arc::clone(catalog)),
        Err(error) => {
            Err(anyhow::Error::new(error.clone())
                .context("load embedded built-in AgentBundle catalog"))
        }
    }
}

pub fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

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
    CompactionConfig {
        token_threshold,
        keep_recent,
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
        .agent_catalog()
        .resolve_agent_entry(stable_agent_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_agent_id.to_string(),
        })?;

    binding.has_selected_bundle_sidecar_capability(stable_agent_id)?;
    let policy = binding.agent_resource_policy(stable_agent_id)?;
    let catalog = binding.agent_catalog();
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
        let (_, _) = binding
            .agent_catalog()
            .resolve_agent_entry(stable_agent_id)
            .ok_or_else(|| CoreError::AgentDefinitionMissing {
                agent_id: stable_agent_id.to_string(),
            })?;
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
        .agent_catalog()
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
                .agent_catalog()
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
        .agent_catalog()
        .resolve_agent_entry(stable_agent_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_agent_id.to_string(),
        })?;
    let policy = binding.agent_resource_policy(stable_agent_id)?;
    let mut expected = BTreeSet::new();
    for stable_id in policy.canonical_hook_ids() {
        let (_, resource) = binding.agent_catalog().resolve_resource_entry(
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

pub struct RuntimeConfig {
    pub router: ProviderRouter,
    pub model: String,
    pub reasoning: Option<ReasoningEffort>,
    pub models: Vec<config::ModelEntry>,
    pub mcp: BTreeMap<String, McpServerConfig>,
    pub plugins: Vec<PluginSpec>,
    pub default_agent: Option<String>,
    /// Logical model categories the runtime resolves at subagent spawn time.
    pub categories: CategoryRegistry,
    /// Set when no usable config was found and the offline provider was chosen.
    /// Interactive startup emits it; headless/machine-readable modes ignore it.
    pub offline_notice: Option<OfflineNotice>,
    pub permission: InvocationPolicy,
    pub websearch: WebSearchConfig,
}

impl RuntimeConfig {
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
    sidecar_environment: &'a BundleSidecarEnvironment,
}

fn authorize_spawn_target<'a>(
    binding: &'a TurnBinding,
    allowed_agents: &[hya_tool::AgentDef],
    caller: &str,
    member: &SpawnMember,
) -> Result<&'a PreparedAgent, SpawnError> {
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
        .any(|allowed| allowed.name == definition.stable_id.as_str())
    {
        return Err(SpawnError::AgentSpawnNotAllowed {
            caller: caller.to_string(),
            agent_id: definition.stable_id.as_str().to_string(),
        });
    }
    Ok(definition)
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
                stable_target: definition.stable_id.clone(),
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
    let authorized_target = definition.stable_id.clone();
    let sidecar_factory = ctx
        .sidecar_environment
        .factory_for(ctx.binding, authorized_target.as_str())
        .map_err(|_| SpawnError::Unavailable)?;
    let mut agent = ctx
        .engine
        .agent_spec_for_binding(ctx.binding, ctx.base, definition.stable_id.as_str())
        .map_err(|_| SpawnError::Unavailable)?;
    let agents = ctx
        .engine
        .agent_roster_for_binding(ctx.binding, definition.stable_id.as_str())
        .map_err(|_| SpawnError::Unavailable)?;
    let resources = ctx
        .engine
        .agent_resource_policy_for_binding(ctx.binding, definition.stable_id.as_str())
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
        if inline.description.is_some() {
            return Err(SpawnError::UnsupportedInlineAgentField {
                field: "description",
            });
        }
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

pub fn spawn_team_supervisor(
    rx: tokio::sync::mpsc::Receiver<BoundSpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    router: Arc<ProviderRouter>,
    categories: Arc<CategoryRegistry>,
    resident_supervisor: Arc<ResidentSupervisor>,
) {
    spawn_team_supervisor_with_environment(
        rx,
        engine,
        base,
        router,
        categories,
        resident_supervisor,
        Arc::new(BundleSidecarEnvironment::production()),
    );
}

fn spawn_team_supervisor_with_environment(
    mut rx: tokio::sync::mpsc::Receiver<BoundSpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    router: Arc<ProviderRouter>,
    categories: Arc<CategoryRegistry>,
    resident_supervisor: Arc<ResidentSupervisor>,
    sidecar_environment: Arc<BundleSidecarEnvironment>,
) {
    tokio::spawn(async move {
        while let Some(bound_request) = rx.recv().await {
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
                    sidecar_environment: &sidecar_environment,
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

pub async fn build_session_engine(
    store: SessionStore,
    router: ProviderRouter,
    agent: &AgentSpec,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
    tool_config: (WebSearchConfig, InvocationPolicy),
) -> anyhow::Result<(
    Arc<SessionEngine>,
    tokio::sync::mpsc::UnboundedReceiver<AskRequest>,
    tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>,
    Arc<dyn hya_server::McpControl>,
    Arc<hya_plugin::PluginHost>,
)> {
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

async fn build_session_engine_with_mcp_defer(
    store: SessionStore,
    router: ProviderRouter,
    agent: &AgentSpec,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
    tool_config: (WebSearchConfig, InvocationPolicy),
    options: EngineBuildOptions,
) -> anyhow::Result<(
    Arc<SessionEngine>,
    tokio::sync::mpsc::UnboundedReceiver<AskRequest>,
    tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>,
    Arc<dyn hya_server::McpControl>,
    Arc<hya_plugin::PluginHost>,
)> {
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
    let catalog = builtin_catalog()?;
    let runtime = Arc::new(RuntimeRegistry::new(registry, Arc::clone(&catalog)));
    let catalog_refresh = Arc::new(InstalledBundleRefresh::new(
        bundle_registry_path(),
        Arc::clone(&catalog),
    ));

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
        .with_compaction(summarizer, compaction_config())
        .with_formatter(formatter_config::load_plane())
        .with_websearch(WebSearchPlane::configured(websearch))
        .with_interaction(interaction)
        .with_spawn_sender(spawn_sender)
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
    spawn_team_supervisor_with_environment(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        spawn_router,
        categories,
        resident_supervisor,
        sidecar_environment,
    );
    // Drive the event-sourced mailbox: append MailSent/Channel*/AgentRegistered to
    // the team-root log and serve roster/channel reads (ADR-0001).
    tokio::spawn(run_mailbox_service(engine.clone(), mailbox_rx));
    Ok((engine, asks, questions, mcp_control, plugin_host))
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

pub struct RuntimeOptions {
    pub model: Option<String>,
    pub db: String,
    pub yolo: bool,
    pub default_agent: Option<String>,
    pub force_offline: bool,
}

pub struct HyaRuntime {
    router: axum::Router,
    engine: Arc<SessionEngine>,
    app_state: hya_server::AppState,
    _plugin_host: Arc<hya_plugin::PluginHost>,
}

impl HyaRuntime {
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
        let (engine, asks, questions, mcp_control, plugin_host) = build_session_engine(
            store,
            runtime.router,
            agent.as_ref(),
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?;
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
        })
    }

    pub fn router(&self) -> &axum::Router {
        &self.router
    }

    #[must_use]
    pub fn engine(&self) -> Arc<SessionEngine> {
        self.engine.clone()
    }

    #[must_use]
    pub fn app_state(&self) -> hya_server::AppState {
        self.app_state.clone()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use async_trait::async_trait;
    use hya_bundle::{
        AgentRole, BundleIdentity, BundleOrigin, BundleSource, HarnessAccess, ModelPolicy,
        PreparedAgent, PreparedBundle, PreparedResource, ResourceView, SourceFile, prepare_package,
    };
    use hya_core::{CategoryEntry, run_team};
    use hya_plugin::messages::{METHOD_TOOL_CALL, ToolCallParams, ToolInfo};
    use hya_plugin::protocol::Frame;
    use hya_proto::{
        Event, FinishReason, MailEndpoint, MailKind, MemberRunStatus, OwnerRunId, RosterStatus,
        SubagentMode, ToolName, ToolSchema,
    };
    use hya_provider::{FakeProvider, FakeStep, HttpProvider, ProviderKind};
    use hya_store::{BundleInstallCandidate, BundleInstallOutcome, BundleRegistry};
    use hya_tool::{
        AgentDef, FormatterPlane, InlineAgent, InteractionPlane, LspPlane, MailboxPlane, Mode,
        PermissionModel, PermissionPlane, PermissionRules, Rule, SkillPlane, SpawnerPlane,
        TodoPlane, Tool, ToolCtx, ToolError, ToolOperation, ToolPermission, ToolRegistry,
        WebSearchPlane,
    };
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::AsyncBufReadExt;
    use tokio_util::sync::CancellationToken;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resident_owner_run_id_is_stable_for_the_process() {
        assert_eq!(process_owner_run_id(), process_owner_run_id());
    }

    fn hex_digest(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        let mut encoded = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            assert!(write!(encoded, "{byte:02x}").is_ok());
        }
        encoded
    }

    #[test]
    fn zero_bundle_prepared_document_cannot_bootstrap_registry_catalog() {
        let bytes = br#"{"format_version":1,"bundles":[],"index":[]}"#;
        let digest = hex_digest(bytes);
        match builtin_catalog_from(bytes, &digest) {
            Ok(catalog) => panic!(
                "zero-bundle prepared must not bootstrap a catalog for RuntimeRegistry, got {} bundles",
                catalog.bundles().len()
            ),
            Err(err) => {
                let message = format!("{err:#}");
                assert!(
                    message.contains("validate embedded built-in AgentBundle catalog"),
                    "empty prepared must surface validate context, got: {message}"
                );
                assert!(
                    message.contains("no bundles") || message.contains("empty"),
                    "empty prepared must fail closed as empty-catalog integrity, got: {message}"
                );
            }
        }
    }

    #[test]
    fn corrupted_prepared_bytes_or_digest_fail_closed_with_decode_context() {
        let wrong_digest =
            builtin_catalog_from(BUILTIN_BUNDLES, "0".repeat(64).as_str()).expect_err("digest");
        let wrong_digest_message = format!("{wrong_digest:#}");
        assert!(
            wrong_digest_message.contains("decode embedded built-in AgentBundle catalog"),
            "digest mismatch must keep decode context, got: {wrong_digest_message}"
        );
        assert!(
            wrong_digest.chain().any(|cause| cause
                .downcast_ref::<hya_bundle::BundleError>()
                .is_some_and(|error| {
                    matches!(
                        error,
                        hya_bundle::BundleError::PreparedDigestMismatch { .. }
                    )
                })),
            "digest mismatch must remain typed BundleError, got: {wrong_digest_message}"
        );

        let garbage = b"not-a-prepared-catalog";
        let garbage_digest = hex_digest(garbage);
        let decode_err = builtin_catalog_from(garbage, &garbage_digest).expect_err("corrupt bytes");
        let decode_message = format!("{decode_err:#}");
        assert!(
            decode_message.contains("decode embedded built-in AgentBundle catalog"),
            "corrupt bytes must keep decode context, got: {decode_message}"
        );
        assert!(
            decode_err.chain().any(|cause| {
                cause
                    .downcast_ref::<hya_bundle::BundleError>()
                    .is_some_and(|error| {
                        matches!(error, hya_bundle::BundleError::PreparedDecode { .. })
                    })
            }),
            "corrupt bytes must remain typed PreparedDecode, got: {decode_message}"
        );
    }

    #[test]
    fn builtin_catalog_initializes_once_and_shares_arc() {
        let first = builtin_catalog().expect("embedded catalog must load");
        let second = builtin_catalog().expect("embedded catalog must load");
        assert!(
            Arc::ptr_eq(&first, &second),
            "embedded prepared catalog must initialize once and be shared"
        );
        assert!(
            !first.bundles().is_empty(),
            "shared embedded catalog must not be empty"
        );
    }

    #[test]
    fn embedded_builtin_catalog_retains_verified_semantic_identity() {
        let catalog = builtin_catalog_from(BUILTIN_BUNDLES, BUILTIN_BUNDLES_DIGEST)
            .expect("embedded catalog must load");
        assert!(
            catalog
                .semantic_identity_v1()
                .is_some_and(|identity| !identity.is_empty()),
            "embedded catalog must retain verified semantic identity"
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
        let workdir = tempdir().join("spawn-admission-workdir-sentinel");
        std::fs::create_dir_all(&workdir).unwrap();
        let engine =
            engine_with_catalog(builtin_catalog().expect("built-in catalog must load")).await;
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
        _lock: std::sync::MutexGuard<'static, ()>,
        home: Option<std::ffi::OsString>,
        xdg_config_home: Option<std::ffi::OsString>,
        xdg_data_home: Option<std::ffi::OsString>,
        current_dir: PathBuf,
    }

    impl EnvGuard {
        fn set(home: &Path, cwd: &Path) -> Self {
            let lock = ENV_LOCK.lock().unwrap();
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
        let (engine, _asks, _questions, _mcp, _plugins) = build_session_engine(
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
        let (engine, _asks, _questions, _mcp, _plugins) = build_session_engine(
            SessionStore::connect_memory().await.unwrap(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();
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
api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: hya/runtime-installed-test
  version: 1.0.0
  publisher: hya
agents:
  - local_id: runtime-installed
    stable_id: runtime-installed-agent
    role: main
    spawn_lifecycle: transient
    harness_access: full
---
You are the runtime-installed agent.
"#,
            )],
        ))
        .unwrap();
        let builtins = builtin_catalog().unwrap();
        let outcome = registry
            .install(
                builtins.bundles(),
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
                builtin_catalog().unwrap(),
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

        let _ = build_session_engine(
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
                    to: MailEndpoint::Handle("queued-1".to_string()),
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
                        agent_type: agent.name.clone(),
                        mode: SubagentMode::Resident,
                    },
                    Event::ResidentWorkStarted {
                        session: running_root,
                        actor_session: running_actor,
                        handle: "running-1".to_string(),
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
        let (_engine, _asks, _questions, _mcp, _plugins) = build_session_engine(
            store.clone(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();

        let running = store.read_projection(running_root).await.unwrap();
        let running_entry = running.team.roster.get("running-1").unwrap();
        assert_eq!(running_entry.status, RosterStatus::Failed);
        assert!(running_entry.resident_work.is_none());

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let queued = store.read_projection(queued_root).await.unwrap();
                let entry = queued.team.roster.get("queued-1").unwrap();
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
api_version: hya.agent-bundle/v1
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
agents:
  - local_id: runtime-installed-resident
    stable_id: runtime-installed-resident-agent
    role: subagent
    spawn_lifecycle: resident
    harness_access: full
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
                builtin_catalog().unwrap().bundles(),
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
                    to: MailEndpoint::Handle("installed-resident-1".to_string()),
                    kind: MailKind::Message,
                    body: "startup recovery executable mail".to_string(),
                },
            )
            .await
            .unwrap();
        let observed_store = store.clone();

        let (engine, _asks, _questions, _mcp, _plugins) = build_session_engine(
            store,
            router,
            &base,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .expect("startup recovery must resolve the installed resident definition");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let projection = observed_store.read_projection(root).await.unwrap();
                let Some(entry) = projection.team.roster.get("installed-resident-1") else {
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
                .stable_id
                .as_str(),
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
        let (_engine, _asks, _questions, mcp_control, _plugins) = result.unwrap();
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
        let (engine, _, _, _, _) = result.unwrap();
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
        let (engine, _, _, control, _) = build_session_engine(
            SessionStore::connect_memory().await.unwrap(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();
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
    async fn engine_with_catalog(catalog: Arc<BundleCatalog>) -> SessionEngine {
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

    fn catalog_with_agents(stable_ids: &[&str]) -> Arc<BundleCatalog> {
        let bundle = PreparedBundle {
            format_version: 1,
            identity: BundleIdentity {
                id: "hya/recovery-resolution".to_string(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            origin: BundleOrigin::Builtin,
            immutable: true,
            digest: "test-only".to_string(),
            agents: stable_ids
                .iter()
                .map(|stable_id| PreparedAgent {
                    local_id: (*stable_id).to_string(),
                    stable_id: AgentName::new(*stable_id),
                    description: None,
                    role: AgentRole::Main,
                    color: None,
                    prompt: Some(format!("{stable_id} recovery prompt")),
                    prompt_source: None,
                    prompt_digest: None,
                    model_policy: ModelPolicy::default(),
                    workdir: None,
                    spawn_lifecycle: SpawnLifecycle::Transient,
                    harness_access: HarnessAccess::Full,
                    resource_view: ResourceView::default(),
                    // Deliberately empty: recovery must not depend on can_spawn.
                    can_spawn: Vec::new(),
                    hook_refs: Vec::new(),
                })
                .collect(),
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
        };
        Arc::new(BundleCatalog::from_prepared(&[bundle]).expect("valid recovery catalog"))
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
    fn catalog_with_worker_policy(model_policy: ModelPolicy) -> Arc<BundleCatalog> {
        let agent = |stable_id: &str, role: AgentRole, can_spawn: &[&str], policy: ModelPolicy| {
            PreparedAgent {
                local_id: stable_id.to_string(),
                stable_id: AgentName::new(stable_id),
                description: None,
                role,
                color: None,
                prompt: Some(format!("{stable_id} prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: policy,
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                harness_access: HarnessAccess::Full,
                resource_view: ResourceView::default(),
                can_spawn: can_spawn.iter().map(|id| AgentName::new(*id)).collect(),
                hook_refs: Vec::new(),
            }
        };
        let bundle = PreparedBundle {
            format_version: 1,
            identity: BundleIdentity {
                id: "hya/spawn-model-precedence".to_string(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            origin: BundleOrigin::Builtin,
            immutable: true,
            digest: "test-only".to_string(),
            agents: vec![
                agent(
                    "build",
                    AgentRole::Main,
                    &["worker"],
                    ModelPolicy::default(),
                ),
                agent("worker", AgentRole::Subagent, &[], model_policy),
            ],
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
        };
        Arc::new(BundleCatalog::from_prepared(&[bundle]).expect("valid precedence catalog"))
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
        let engine = engine_with_catalog(Arc::new(
            BundleCatalog::from_prepared(&[materialized_bundle("executable")])
                .expect("executable fixture catalog"),
        ))
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
        bundle.agents[0].hook_refs = vec![
            event_hook_id,
            before_hook.stable_id.clone(),
            after_hook.stable_id.clone(),
        ];
        bundle.hooks.extend([before_hook, after_hook]);
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle]).expect("activation hook fixture catalog"),
        );
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[disjoint_materialized_bundle("selected-hooks")])
                .expect("selected-hooks fixture catalog"),
        );
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[materialized_tool_bundle("shutdown")])
                .expect("shutdown fixture catalog"),
        );
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[materialized_tool_bundle("drop-cleanup")])
                .expect("drop cleanup fixture catalog"),
        );
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[materialized_tool_bundle("cancel-start")])
                .expect("cancel-start fixture catalog"),
        );
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[materialized_tool_bundle("loss-token")])
                .expect("loss token fixture catalog"),
        );
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
api_version: hya.agent-bundle/v1
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
agents:
  - local_id: main
    stable_id: directory-helper-main
    role: main
    spawn_lifecycle: transient
    harness_access: full
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(prepared.bundles())
                .expect("build directory-authored Bundle catalog"),
        );
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle]).expect("Bun extension fixture catalog"),
        );
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
        let mut bundle = disjoint_materialized_bundle("bun-disjoint");
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
      },
      event: async () => {},
    }
  },
}
"#
            .to_string(),
        );
        set_materialized_extension_content_for_path(
            &mut bundle,
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

        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle])
                .expect("disjoint Bun entrypoint fixture catalog"),
        );
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
            vec!["bundle:hya/materialized/tool/beta"],
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

        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle]).expect("generic Bun superset fixture catalog"),
        );
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[materialized_bun_bundle("bun-e2e")])
                .expect("Bun E2E fixture catalog"),
        );
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
        bundle.agents[0].hook_refs = vec![
            event_hook.stable_id.clone(),
            before_hook.stable_id.clone(),
            after_hook.stable_id.clone(),
        ];
        bundle.hooks.extend([event_hook, before_hook, after_hook]);
        bundle.agents[0].local_id = "build".to_string();
        bundle.agents[0].stable_id = AgentName::new("build");
        bundle.agents[0].role = AgentRole::Main;
        bundle.agents[0].spawn_lifecycle = SpawnLifecycle::Transient;
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle]).expect("root Bun hook fixture catalog"),
        );
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
                agent: AgentName::new("build"),
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
                    name: AgentName::new("build"),
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
        bundle.agents[0].spawn_lifecycle = SpawnLifecycle::Resident;
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle]).expect("resident Bun fixture catalog"),
        );
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
        bundle.agents[0].spawn_lifecycle = SpawnLifecycle::Resident;
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle]).expect("resident loss fixture catalog"),
        );
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

        let replacement = Arc::new(
            BundleCatalog::from_prepared(&[materialized_bundle("resident-replacement")])
                .expect("replacement resident catalog"),
        );
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
        bundle.agents[0].spawn_lifecycle = SpawnLifecycle::Resident;
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle]).expect("resident running loss fixture catalog"),
        );
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
        let replacement = Arc::new(
            BundleCatalog::from_prepared(&[materialized_bundle("resident-running-replacement")])
                .expect("replacement resident running catalog"),
        );
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[materialized_bundle("unsupported-hook")])
                .expect("unsupported hook fixture catalog"),
        );
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
            origin: BundleOrigin::Builtin,
            immutable: true,
            digest: format!("{marker}-bundle-digest"),
            agents: vec![PreparedAgent {
                local_id: "worker".to_string(),
                stable_id: AgentName::new("worker"),
                description: None,
                role: AgentRole::Subagent,
                color: None,
                prompt: Some(format!("{marker} worker prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                harness_access: HarnessAccess::Full,
                resource_view,
                can_spawn: Vec::new(),
                hook_refs: vec!["bundle:hya/materialized/hook/event".to_string()],
            }],
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
        for agent in &mut bundle.agents {
            agent.hook_refs.clear();
        }
        bundle
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
            origin: BundleOrigin::Builtin,
            immutable: true,
            digest: format!("{marker}-disjoint-bundle-digest"),
            agents: vec![
                PreparedAgent {
                    local_id: "alpha".to_string(),
                    stable_id: AgentName::new("alpha"),
                    description: None,
                    role: AgentRole::Subagent,
                    color: None,
                    prompt: Some(format!("{marker} alpha prompt")),
                    prompt_source: None,
                    prompt_digest: None,
                    model_policy: ModelPolicy::default(),
                    workdir: None,
                    spawn_lifecycle: SpawnLifecycle::Transient,
                    harness_access: HarnessAccess::Full,
                    resource_view: ResourceView {
                        allow: vec![alpha_tool.stable_id.clone()],
                        ..ResourceView::default()
                    },
                    can_spawn: Vec::new(),
                    hook_refs: vec![alpha_hook.stable_id.clone()],
                },
                PreparedAgent {
                    local_id: "beta".to_string(),
                    stable_id: AgentName::new("beta"),
                    description: None,
                    role: AgentRole::Subagent,
                    color: None,
                    prompt: Some(format!("{marker} beta prompt")),
                    prompt_source: None,
                    prompt_digest: None,
                    model_policy: ModelPolicy::default(),
                    workdir: None,
                    spawn_lifecycle: SpawnLifecycle::Transient,
                    harness_access: HarnessAccess::Full,
                    resource_view: ResourceView {
                        allow: vec![beta_tool.stable_id.clone()],
                        ..ResourceView::default()
                    },
                    can_spawn: Vec::new(),
                    hook_refs: vec![beta_hook.stable_id.clone()],
                },
            ],
            tools: vec![alpha_tool, beta_tool],
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: vec![alpha_hook, beta_hook],
            extensions: vec![alpha_extension, beta_extension],
        }
    }

    fn cross_bundle_selector_catalog(marker: &str) -> Arc<BundleCatalog> {
        let owner = materialized_bundle(marker);
        let mut selector = owner.clone();
        selector.identity.id = "hya/selector".to_string();
        selector.digest = "selector-bundle-digest".to_string();
        let mut selector_agent = selector.agents[0].clone();
        selector_agent.local_id = "selector".to_string();
        selector_agent.stable_id = AgentName::new("selector");
        selector_agent.resource_view = ResourceView::default();
        selector_agent.hook_refs = vec!["bundle:hya/materialized/hook/event".to_string()];
        selector.agents = vec![selector_agent];
        selector.tools.clear();
        selector.skills.clear();
        selector.mcp.clear();
        selector.hooks.clear();
        selector.extensions.clear();

        Arc::new(
            BundleCatalog::from_prepared(&[selector, owner])
                .expect("cross-bundle materialization fixture catalog"),
        )
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
        let old_catalog = Arc::new(
            BundleCatalog::from_prepared(&[materialized_bundle("old")])
                .expect("old fixture catalog"),
        );
        let new_catalog = Arc::new(
            BundleCatalog::from_prepared(&[materialized_bundle("new")])
                .expect("new fixture catalog"),
        );
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
        let old_catalog = Arc::new(
            BundleCatalog::from_prepared(&[disjoint_materialized_bundle("old")])
                .expect("old disjoint fixture catalog"),
        );
        let new_catalog = Arc::new(
            BundleCatalog::from_prepared(&[disjoint_materialized_bundle("new")])
                .expect("new disjoint fixture catalog"),
        );
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
        bundle.agents[0]
            .resource_view
            .allow
            .push(bundle.tools[1].stable_id.clone());
        bundle.extensions[0].local_id = "zeta".to_string();
        bundle.extensions[0].stable_id = "bundle:hya/materialized/extension/zeta".to_string();
        bundle.extensions[1].local_id = "alpha".to_string();
        bundle.extensions[1].stable_id = "bundle:hya/materialized/extension/alpha".to_string();

        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle])
                .expect("ordered materialization fixture catalog"),
        );
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

    #[test]
    fn sidecar_materialization_joins_cross_bundle_hook_to_owner_extension() {
        let catalog = cross_bundle_selector_catalog("owner");
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture cross-bundle materialization turn binding");
        let activation_root = tempdir();
        let activation_dir = activation_root.join("activation");
        std::fs::create_dir_all(&activation_dir).expect("create activation directory");

        let materialized =
            materialize_bundle_sidecar_resources(&binding, "selector", &activation_dir);
        let extension_content =
            std::fs::read_to_string(activation_dir.join("extensions/runtime.js")).ok();
        std::fs::remove_dir_all(&activation_root).expect("cleanup activation directory");
        std::fs::remove_dir_all(&turn_dir).expect("cleanup turn directory");

        let materialized = materialized.expect("materialize owner bundle extension");
        assert_eq!(
            materialized,
            vec![activation_dir.join("extensions/runtime.js")]
        );
        assert_eq!(
            extension_content.as_deref(),
            Some("owner extensions/runtime.js\n")
        );
    }

    #[test]
    fn sidecar_materialization_separates_same_relative_path_across_owning_bundles() {
        let owner_bundle = |bundle_id: &str, marker: &str| {
            let mut bundle = materialized_tool_bundle(marker);
            bundle.identity.id = bundle_id.to_string();
            bundle.digest = format!("{marker}-bundle-digest");
            bundle.agents.clear();
            bundle.tools[0].stable_id = format!("bundle:{bundle_id}/tool/echo");
            bundle.extensions[0].stable_id = format!("bundle:{bundle_id}/extension/runtime");
            set_materialized_extension_content(
                &mut bundle,
                format!("{marker} extensions/runtime.js\n"),
            );
            bundle
        };
        let owner_a = owner_bundle("hya/owner-a", "owner-a");
        let owner_b = owner_bundle("hya/owner-b", "owner-b");
        let owner_a_tool_id = owner_a.tools[0].stable_id.clone();
        let owner_b_tool_id = owner_b.tools[0].stable_id.clone();

        let mut selector = materialized_tool_bundle("selector");
        selector.identity.id = "hya/selector".to_string();
        selector.digest = "selector-bundle-digest".to_string();
        let mut selector_agent = selector.agents[0].clone();
        selector_agent.local_id = "selector".to_string();
        selector_agent.stable_id = AgentName::new("selector");
        selector_agent.resource_view = ResourceView {
            allow: vec![owner_a_tool_id, owner_b_tool_id],
            ..ResourceView::default()
        };
        selector_agent.hook_refs.clear();
        selector.agents = vec![selector_agent];
        selector.tools.clear();
        selector.skills.clear();
        selector.mcp.clear();
        selector.hooks.clear();
        selector.extensions.clear();

        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[selector, owner_a, owner_b])
                .expect("same-path cross-bundle materialization fixture catalog"),
        );
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture same-path cross-bundle turn binding");
        let activation_root = tempdir();
        let activation_dir = activation_root.join("activation");
        std::fs::create_dir_all(&activation_dir).expect("create activation directory");

        let materialized =
            materialize_bundle_sidecar_resources(&binding, "selector", &activation_dir);
        let observed_contents = materialized.as_ref().ok().map(|paths| {
            paths
                .iter()
                .map(|path| std::fs::read_to_string(path).expect("read owner extension"))
                .collect::<Vec<_>>()
        });
        std::fs::remove_dir_all(&activation_root).expect("cleanup activation directory");
        std::fs::remove_dir_all(&turn_dir).expect("cleanup turn directory");

        let materialized = materialized.expect("materialize same-path owner extensions");
        let observed_contents = observed_contents.expect("read same-path owner extensions");
        assert_eq!(materialized.len(), 2);
        assert_ne!(materialized[0], materialized[1]);
        assert!(
            materialized
                .iter()
                .all(|path| path.starts_with(&activation_dir))
        );
        assert_eq!(
            observed_contents,
            vec![
                "owner-a extensions/runtime.js\n",
                "owner-b extensions/runtime.js\n",
            ]
        );
    }

    #[test]
    fn cross_bundle_selected_tool_uses_captured_owner_extension() {
        let owner = materialized_tool_bundle("cross-tool-owner");
        let owner_tool_id = owner.tools[0].stable_id.clone();
        let mut selector = owner.clone();
        selector.identity.id = "hya/selector".to_string();
        selector.digest = "selector-cross-tool-digest".to_string();
        let mut selector_agent = selector.agents[0].clone();
        selector_agent.local_id = "selector".to_string();
        selector_agent.stable_id = AgentName::new("selector");
        selector_agent.resource_view = ResourceView {
            allow: vec![owner_tool_id.clone()],
            ..ResourceView::default()
        };
        selector_agent.hook_refs.clear();
        selector.agents = vec![selector_agent];
        selector.tools.clear();
        selector.skills.clear();
        selector.mcp.clear();
        selector.hooks.clear();
        selector.extensions.clear();

        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[selector, owner])
                .expect("cross-bundle tool materialization fixture catalog"),
        );
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture cross-bundle tool turn binding");
        let policy = binding
            .agent_resource_policy("selector")
            .expect("resolve cross-bundle selected tool policy");
        assert_eq!(policy.selected_bundle_tool_ids(), &[owner_tool_id]);

        let staging_root = tempdir();
        let environment = BundleSidecarEnvironment::from_command(
            vec!["bun".to_string(), "sidecar.js".to_string()],
            staging_root.clone(),
        );
        let factory = environment
            .factory_for(&binding, "selector")
            .expect("resolve cross-bundle selected tool sidecar factory");
        assert!(factory.is_some());

        let activation_root = tempdir();
        let activation_dir = activation_root.join("activation");
        std::fs::create_dir_all(&activation_dir).expect("create activation directory");
        let materialized =
            materialize_bundle_sidecar_resources(&binding, "selector", &activation_dir)
                .expect("materialize captured owner tool extension");
        assert_eq!(
            materialized,
            vec![activation_dir.join("extensions/runtime.js")]
        );
        assert_eq!(
            std::fs::read_to_string(activation_dir.join("extensions/runtime.js"))
                .expect("read owner extension"),
            "cross-tool-owner extensions/runtime.js\n"
        );

        std::fs::remove_dir_all(activation_root).expect("cleanup activation directory");
        std::fs::remove_dir_all(staging_root).expect("cleanup cross-bundle tool staging root");
        std::fs::remove_dir_all(turn_dir).expect("cleanup cross-bundle tool turn directory");
    }

    #[test]
    fn sidecar_factory_is_enabled_by_cross_bundle_selected_hook() {
        let catalog = cross_bundle_selector_catalog("owner");
        let runtime = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let turn_dir = tempdir();
        let binding = runtime
            .bind_turn(&turn_dir)
            .expect("capture cross-bundle factory turn binding");
        let staging_root = tempdir();
        let environment = BundleSidecarEnvironment::from_command(
            vec!["bun".to_string(), "sidecar.js".to_string()],
            staging_root.clone(),
        );

        let factory = environment
            .factory_for(&binding, "selector")
            .expect("resolve cross-bundle selected-hook sidecar factory");

        std::fs::remove_dir_all(staging_root).expect("cleanup cross-bundle staging root");
        std::fs::remove_dir_all(turn_dir).expect("cleanup cross-bundle turn directory");

        assert!(
            factory.is_some(),
            "selected cross-bundle hook must expose a sidecar factory"
        );
    }

    #[tokio::test]
    async fn bundle_sidecar_tool_declaration_binds_canonical_captured_resource() {
        let old_catalog = Arc::new(
            BundleCatalog::from_prepared(&[materialized_bundle("old")])
                .expect("old fixture catalog"),
        );
        let mut new_bundle = materialized_bundle("new");
        new_bundle.tools.clear();
        let new_catalog =
            Arc::new(BundleCatalog::from_prepared(&[new_bundle]).expect("new fixture catalog"));
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
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[disjoint_materialized_bundle("selected-tools")])
                .expect("selected-tools fixture catalog"),
        );
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
        bundle.agents[0].resource_view.allow = selected_tool_ids;
        bundle.agents[0].hook_refs = selected_hook_ids;

        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle])
                .expect("order-independent declaration fixture catalog"),
        );
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
        bundle.agents[0].resource_view = ResourceView::default();
        let catalog =
            Arc::new(BundleCatalog::from_prepared(&[bundle]).expect("hook-only fixture catalog"));
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
        let mut bundle = materialized_bundle("selected-agent");
        let mut worker = bundle.agents.pop().expect("materialized worker fixture");
        worker.spawn_lifecycle = SpawnLifecycle::Resident;
        let mut build_resource_view = ResourceView::default();
        build_resource_view
            .deny
            .push("bundle:hya/materialized/tool/echo".to_string());
        let build = PreparedAgent {
            local_id: "build".to_string(),
            stable_id: AgentName::new("build"),
            description: None,
            role: AgentRole::Main,
            color: None,
            prompt: Some("selected-agent build prompt".to_string()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            spawn_lifecycle: SpawnLifecycle::Transient,
            harness_access: HarnessAccess::None,
            resource_view: build_resource_view,
            can_spawn: vec![AgentName::new("worker")],
            hook_refs: Vec::new(),
        };
        bundle.agents = vec![build, worker];

        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle]).expect("selected-agent fixture catalog"),
        );
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
        let executable_catalog = Arc::new(
            BundleCatalog::from_prepared(&[materialized_bundle("executable")])
                .expect("executable fixture catalog"),
        );
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
        for agent in &mut static_bundle.agents {
            agent.hook_refs.clear();
            agent.resource_view = ResourceView::default();
        }
        let static_catalog = Arc::new(
            BundleCatalog::from_prepared(&[static_bundle]).expect("static fixture catalog"),
        );
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
}
