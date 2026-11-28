use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use hya_engine::{
    BoundSidecarFactory, CoreError, SidecarEnvironment, SidecarHandle, SidecarLifecycle,
    SidecarStart, TurnBinding,
};
use hya_plugin::PluginContributionSet;
use hya_plugin::config::{PluginSpec, ProcessLaunch};
use hya_plugin::messages::{ActivationLifecycle, ActivationMetadata, PluginKindWire};
#[cfg(test)]
use hya_tool::ResolvedTool;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

use super::{
    ActivationBindings, bind_bundle_sidecar_tools, materialize_bundle_sidecar_resources,
    start_activation_sidecar, validate_bundle_sidecar_hooks,
};
use crate::bundle_runtime::source::adapt_prepared_bundle_skills;
use crate::plugin_adapter::PluginHostAdapter;
use crate::runtime::host_info;
use crate::{bundle_registry_path, plugins};

pub(crate) struct BundleSidecarEnvironment {
    io_tasks: Option<tokio_util::task::TaskTracker>,
    command: Option<Vec<String>>,
    staging_root: PathBuf,
    #[cfg(test)]
    pub(crate) terminate_notify: Option<Arc<tokio::sync::Notify>>,
}

impl BundleSidecarEnvironment {
    #[cfg(test)]
    pub(crate) fn from_command(command: Vec<String>, staging_root: PathBuf) -> Self {
        Self {
            io_tasks: hya_child_io::io_tasks::current(),
            command: Some(command),
            staging_root,
            terminate_notify: None,
        }
    }

    pub(crate) fn production(io_tasks: tokio_util::task::TaskTracker) -> Self {
        let registry_path = bundle_registry_path();
        let staging_root = registry_path.parent().map_or_else(
            || PathBuf::from("activations"),
            |parent| parent.join("activations"),
        );
        Self {
            io_tasks: Some(io_tasks),
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
        let has_selected_sidecar_capability = binding
            .has_selected_bundle_sidecar_capability(stable_agent_id)
            .map_err(CoreError::bundle)?;
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

#[async_trait::async_trait]
impl BoundSidecarFactory for BundleSidecarFactory {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        let handle = start_activation_sidecar(
            start, &self.command, &self.staging_root, host_info(), self.io_tasks.clone(),
            |start, activation_dir| {
                let extension_paths = materialize_bundle_sidecar_resources(
                    &self.binding,
                    &self.stable_agent_id,
                    activation_dir,
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
                Ok(PluginSpec {
                    id: self.stable_agent_id.clone(),
                    kind: PluginKindWire::Bun,
                    command,
                    timeout_ms: None,
                    env: environment,
                    plugin_dir: None,
                    launch: ProcessLaunch::Bundle {
                        root: activation_dir.to_path_buf(),
                        activation: Some(ActivationMetadata {
                            activation_id: start.activation_id.clone(),
                            lifecycle,
                        }),
                    },
                })
            },
            |plugin, host| {
                let contributions = plugin.contributions();
                validate_bundle_sidecar_hooks(
                    &self.binding,
                    &self.stable_agent_id,
                    &contributions.hooks,
                )?;
                validate_bundle_sidecar_skills(&self.binding, &self.stable_agent_id, contributions)?;
                let tools = bind_bundle_sidecar_tools(&self.binding, &self.stable_agent_id, plugin)?;
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
                    .sidecar_hook_view(&self.stable_agent_id, Arc::new(PluginHostAdapter::new(host)))
                    .map_err(CoreError::bundle)?;
                Ok(ActivationBindings { tools, hooks })
            },
        ).await?;
        #[cfg(test)]
        let handle: Box<dyn SidecarHandle> = match &self.terminate_notify {
            Some(notify) => Box::new(NotifySidecarHandle {
                inner: handle,
                notify: notify.clone(),
            }),
            None => handle,
        };
        Ok(handle)
    }
}

/// Require a bundle sidecar's Skill declaration set to match signed prepared bytes.
pub(crate) fn validate_bundle_sidecar_skills(
    binding: &TurnBinding,
    stable_agent_id: &str,
    contributions: &PluginContributionSet,
) -> Result<(), CoreError> {
    let policy = binding
        .agent_resource_policy(stable_agent_id)
        .map_err(CoreError::bundle)?;
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

#[cfg(test)]
/// Test-only notification at the adapter's completed termination boundary.
struct NotifySidecarHandle {
    inner: Box<dyn SidecarHandle>,
    notify: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
#[async_trait::async_trait]
impl SidecarHandle for NotifySidecarHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.inner.ready().await
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.inner.shutdown().await
    }
    fn is_healthy(&self) -> bool {
        self.inner.is_healthy()
    }
    fn loss_token(&self) -> Option<CancellationToken> {
        self.inner.loss_token()
    }
    async fn terminate(&mut self) -> Result<(), CoreError> {
        let result = self.inner.terminate().await;
        self.notify.notify_one();
        result
    }
    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        self.inner.tool_bindings()
    }
    fn hook_dispatcher(&self) -> Option<hya_engine::HookEntry> {
        self.inner.hook_dispatcher()
    }
}
