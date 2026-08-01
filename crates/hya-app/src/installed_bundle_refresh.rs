use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{BundleCatalog, BundleOrigin, PreparedBundle, PreparedCatalog};
use hya_core::{CoreError, RuntimeCatalogRefresh, RuntimeRegistry};
use hya_store::BundleRegistry;
use tokio::sync::{Mutex, OnceCell};

#[must_use]
pub fn bundle_registry_path() -> PathBuf {
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(data_home).join("hya/bundles/registry.sqlite3");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home).join(".local/share/hya/bundles/registry.sqlite3");
    }
    PathBuf::from(".local/share/hya/bundles/registry.sqlite3")
}

/// Lazily publishes installed Bundle catalog changes at root binding boundaries.
pub struct InstalledBundleRefresh {
    registry_path: PathBuf,
    builtins: Vec<PreparedBundle>,
    registry: OnceCell<BundleRegistry>,
    applied_generation: Mutex<u64>,
}

impl InstalledBundleRefresh {
    #[must_use]
    pub fn new(registry_path: PathBuf, builtins: Vec<PreparedBundle>) -> Self {
        Self {
            registry_path,
            builtins,
            registry: OnceCell::new(),
            applied_generation: Mutex::new(0),
        }
    }

    pub async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        if self.registry.get().is_none()
            && !self.registry_path.try_exists().map_err(|error| {
                CoreError::Invalid(format!("inspect installed Bundle registry path: {error}"))
            })?
        {
            return Ok(false);
        }
        let path = self.registry_path.to_str().ok_or_else(|| {
            CoreError::Invalid("installed Bundle registry path is not UTF-8".to_string())
        })?;
        let registry = self
            .registry
            .get_or_try_init(|| async { BundleRegistry::connect(path).await })
            .await?;
        let generation = registry.generation().await?;
        let mut applied_generation = self.applied_generation.lock().await;
        if generation == *applied_generation {
            return Ok(false);
        }

        let snapshot = registry.snapshot().await?;
        if snapshot.generation == *applied_generation {
            return Ok(false);
        }
        let mut bundles = self.builtins.clone();
        for record in snapshot.bundles {
            let prepared =
                PreparedCatalog::decode(&record.prepared_bytes, &record.prepared_digest)?;
            let [bundle] = prepared.bundles() else {
                return Err(CoreError::Invalid(format!(
                    "installed Bundle `{}` prepared catalog must contain exactly one bundle",
                    record.bundle_id
                )));
            };
            if bundle.origin != BundleOrigin::Installed
                || bundle.immutable
                || bundle.identity.id != record.bundle_id
                || bundle.identity.version != record.version
                || bundle.identity.publisher != record.publisher
            {
                return Err(CoreError::Invalid(format!(
                    "installed Bundle `{}` metadata does not match its prepared catalog",
                    record.bundle_id
                )));
            }
            bundles.push(bundle.clone());
        }
        let catalog = BundleCatalog::from_prepared(&bundles)?;
        runtime.publish_catalog(Arc::new(catalog))?;
        *applied_generation = snapshot.generation;
        Ok(true)
    }
}

#[async_trait]
impl RuntimeCatalogRefresh for InstalledBundleRefresh {
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        InstalledBundleRefresh::refresh_if_changed(self, runtime).await
    }
}
