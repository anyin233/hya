//! Activation process ownership behind the core sidecar interfaces.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use hya_core::{CoreError, SidecarHandle, SidecarStart};
use hya_tool::ResolvedTool;
use tokio_util::task::TaskTracker;

use crate::config::PluginSpec;
use crate::messages::{HookName, HookRegistration};
use crate::{HostInfo, PluginHost, PreparedPlugin};
use hya_core::TurnBinding;
use hya_tool::ToolPermission;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write;

/// Capabilities selected from a completed activation handshake.
pub struct ActivationBindings {
    /// Tools exposed to the activated agent.
    pub tools: Arc<[ResolvedTool]>,
    /// Hooks exposed to the activated agent.
    pub hooks: Option<Arc<dyn hya_core::hooks::HookDispatcher>>,
}

/// Initialize one private activation, validate its selected exports, and retain its process owner.
/// The application supplies launch configuration and capability selection from its bound catalog.
pub async fn start_activation_sidecar(
    start: SidecarStart,
    command: &[String],
    staging_root: &Path,
    host_info: HostInfo,
    io_tasks: Option<TaskTracker>,
    launch: impl FnOnce(&SidecarStart, &Path) -> Result<PluginSpec, CoreError>,
    bind: impl FnOnce(&PreparedPlugin, Arc<PluginHost>) -> Result<ActivationBindings, CoreError>,
) -> Result<Box<dyn SidecarHandle>, CoreError> {
    validate_activation_id(&start.activation_id)?;
    if command.is_empty() {
        return Err(CoreError::Invalid(
            "bundle sidecar command is empty".to_string(),
        ));
    }
    std::fs::create_dir_all(staging_root).map_err(|error| {
        CoreError::Invalid(format!(
            "create bundle sidecar staging root `{}`: {error}",
            staging_root.display()
        ))
    })?;
    let activation_dir = staging_root.join(&start.activation_id);
    std::fs::create_dir(&activation_dir).map_err(|error| {
        CoreError::Invalid(format!(
            "create bundle sidecar activation directory `{}`: {error}",
            activation_dir.display()
        ))
    })?;
    let activation_dir = BundleActivationDirGuard::new(activation_dir);
    let spec = launch(&start, activation_dir.path())?;
    let host = Arc::new(
        hya_mcp::io_tasks::scope(io_tasks, PluginHost::connect(spec, host_info, None))
            .await
            .map_err(|error| CoreError::Invalid(format!("initialize bundle sidecar: {error}")))?,
    );
    let plugin = host.prepared_plugins().remove(0);
    let ActivationBindings { tools, hooks } = match bind(&plugin, host.clone()) {
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
    }))
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

struct BundleSidecarHandle {
    host: Option<Arc<PluginHost>>,
    loss: tokio_util::sync::CancellationToken,
    activation_dir: Option<PathBuf>,
    tools: Arc<[ResolvedTool]>,
    hooks: Option<Arc<dyn hya_core::hooks::HookDispatcher>>,
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
        terminate_error.or(cleanup_error).map_or(Ok(()), Err)
    }

    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        Arc::clone(&self.tools)
    }

    fn hook_dispatcher(&self) -> Option<Arc<dyn hya_core::hooks::HookDispatcher>> {
        self.hooks.clone()
    }
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

/// Materialize the selected captured bundle extension closure into an activation directory.
pub fn materialize_bundle_sidecar_resources(
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

/// Validate the complete module tool declaration and bind the selected agent view.
pub fn bind_bundle_sidecar_tools(
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

/// Require the complete selected-module hook declaration before publishing its view.
pub fn validate_bundle_sidecar_hooks(
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
