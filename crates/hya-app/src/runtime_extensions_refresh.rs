use std::path::{Path, PathBuf};
use std::sync::Arc;

use hya_engine::RuntimeRegistry;

use crate::runtime_mcp_control::RuntimeMcpControl;
use crate::runtime_reconcile::RuntimeReconciler;

/// Refresh config plugins at the same bind/explicit-refresh boundary as bundles.
pub(crate) struct RuntimeExtensionsRefresh {
    catalogs: crate::ProjectScopeRefresh,
    reconciler: Arc<RuntimeReconciler>,
    config_file: PathBuf,
    applied: tokio::sync::Mutex<Option<String>>,
    pure: bool,
}

impl RuntimeExtensionsRefresh {
    pub(crate) fn new(
        catalogs: crate::ProjectScopeRefresh,
        reconciler: Arc<RuntimeReconciler>,
        pure: bool,
    ) -> Result<Self, hya_engine::CoreError> {
        let config_file = crate::config::active_config_path();
        let applied = read_plugin_config(&config_file)?;
        Ok(Self {
            catalogs,
            reconciler,
            config_file,
            applied: tokio::sync::Mutex::new(applied),
            pure,
        })
    }
}

fn read_plugin_config(path: &Path) -> Result<Option<String>, hya_engine::CoreError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(hya_engine::CoreError::Invalid(format!(
            "read plugin config {}: {error}",
            path.display()
        ))),
    }
}

impl RuntimeExtensionsRefresh {
    async fn refresh_plugins(&self, explicit: bool) -> Result<(), hya_engine::CoreError> {
        if self.pure {
            return Ok(());
        }
        let mut applied = self.applied.lock().await;
        let loaded = match read_plugin_config(&self.config_file) {
            Ok(next) => {
                if !explicit && *applied == next {
                    return Ok(());
                }
                // Remember failed inputs too; only edits or explicit refresh retry.
                *applied = next.clone();
                crate::config::parse_plugin_config(next.as_deref().unwrap_or("")).map_err(|error| {
                    hya_engine::CoreError::Invalid(format!("load plugin config: {error:#}"))
                })
            }
            Err(error) => Err(error),
        };
        let (specs, config_error) = match loaded {
            Ok(specs) => (Some(specs), None),
            Err(error) if explicit => (None, Some(error)),
            Err(error) => return Err(error),
        };
        let plan = self
            .reconciler
            .replace_plugins(specs, explicit)
            .map_err(|error| hya_engine::CoreError::Invalid(error.to_string()))?;
        let result = RuntimeMcpControl::new(self.reconciler.clone())
            .reconcile_plan(plan, tokio_util::sync::CancellationToken::new())
            .await
            .map(|_| ())
            .map_err(|error| hya_engine::CoreError::Invalid(error.to_string()));
        if let Some(error) = config_error {
            return Err(error);
        }
        result
    }
}

#[async_trait::async_trait]
impl hya_engine::RuntimeCatalogRefresh for RuntimeExtensionsRefresh {
    async fn refresh_if_changed(
        &self,
        runtime: &RuntimeRegistry,
    ) -> Result<bool, hya_engine::CoreError> {
        let before = runtime.effective_manifest().generation;
        let catalogs = self.catalogs.refresh_if_changed(runtime).await;
        let plugins = self.refresh_plugins(false).await;
        catalogs?;
        plugins?;
        Ok(runtime.effective_manifest().generation != before)
    }

    async fn refresh_explicit(
        &self,
        runtime: &RuntimeRegistry,
    ) -> Result<bool, hya_engine::CoreError> {
        let before = runtime.effective_manifest().generation;
        let catalogs = self.catalogs.refresh_explicit(runtime).await;
        let plugins = self.refresh_plugins(true).await;
        catalogs?;
        plugins?;
        Ok(runtime.effective_manifest().generation != before)
    }

    async fn refresh_scope(
        &self,
        runtime: &RuntimeRegistry,
        scope: &hya_engine::CatalogScope,
    ) -> Result<bool, hya_engine::CoreError> {
        self.catalogs.refresh_scope(runtime, scope).await
    }

    async fn refresh_scope_explicit(
        &self,
        runtime: &RuntimeRegistry,
        scope: &hya_engine::CatalogScope,
    ) -> Result<bool, hya_engine::CoreError> {
        self.catalogs.refresh_scope_explicit(runtime, scope).await
    }
}

#[cfg(test)]
mod tests;
