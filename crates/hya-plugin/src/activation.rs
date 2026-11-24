//! Activation process ownership behind the core sidecar interfaces.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use hya_engine::{CoreError, SidecarHandle, SidecarStart};
use hya_tool::ResolvedTool;
use tokio_util::task::TaskTracker;

use crate::config::PluginSpec;
use crate::{HostInfo, PluginHost, PreparedPlugin};

/// Capabilities selected from a completed activation handshake.
pub struct ActivationBindings {
    /// Tools exposed to the activated agent.
    pub tools: Arc<[ResolvedTool]>,
    /// Hooks exposed to the activated agent.
    pub hooks: Option<Arc<dyn hya_engine::hooks::HookDispatcher>>,
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
        hya_child_io::io_tasks::scope(io_tasks, PluginHost::connect(spec, host_info, None))
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
    hooks: Option<Arc<dyn hya_engine::hooks::HookDispatcher>>,
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

    fn hook_dispatcher(&self) -> Option<Arc<dyn hya_engine::hooks::HookDispatcher>> {
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
