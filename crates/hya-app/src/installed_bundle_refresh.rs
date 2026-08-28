use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{BundleCatalog, PreparedCatalog};
use hya_core::{
    AgentCatalog, CoreError, RuntimeCatalogRefresh, RuntimeRegistry, RuntimeSource,
    RuntimeSourceKind,
};
use hya_plugin::{PluginContributionSet, SkillContribution};
use hya_store::{BundleRegistry, BundleRegistryRecord};
use tokio::sync::{Mutex, OnceCell};

use crate::runtime_reconcile::prepared_static_bundle_source;

/// Decode the build-prepared first-party WorkflowBundle.
pub fn first_party_catalog() -> Result<PreparedCatalog, CoreError> {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/first-party.prepared.json"));
    let digest = include_str!(concat!(env!("OUT_DIR"), "/first-party.prepared.digest")).trim();
    Ok(PreparedCatalog::decode(bytes, digest)?)
}

/// Default path of the installed Bundle registry SQLite file.
///
/// Uses `$XDG_DATA_HOME/hya/bundles/registry.sqlite3`, else
/// `$HOME/.local/share/hya/bundles/registry.sqlite3`, else a cwd-relative fallback.
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
    registry: OnceCell<BundleRegistry>,
    applied_generation: Mutex<u64>,
}

impl InstalledBundleRefresh {
    /// Track installed-catalog generations for `registry_path`.
    #[must_use]
    pub fn new(registry_path: PathBuf) -> Self {
        Self {
            registry_path,
            registry: OnceCell::new(),
            applied_generation: Mutex::new(0),
        }
    }

    /// Publish a new installed catalog generation when the registry advanced.
    ///
    /// Returns `Ok(true)` if the runtime registry was updated, `Ok(false)` when
    /// the path is missing or the generation is unchanged.
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
        // A row written by a different binary version cannot decode. Skip it
        // with a named warning and keep the rest of the catalog usable: a single
        // stale row must not wedge every later turn, and the operator needs to
        // know which bundle to reinstall.
        let mut prepared_catalogs = Vec::with_capacity(snapshot.bundles.len());
        for record in snapshot.bundles {
            match Self::decode_installed(&record) {
                Ok(prepared) => prepared_catalogs.push(prepared),
                Err(detail) => {
                    tracing::warn!(
                        bundle_id = %record.bundle_id,
                        version = %record.version,
                        error = %detail,
                        "skipping unreadable installed bundle; reinstall it with `hya bundle install`"
                    );
                }
            }
        }
        let first_party = first_party_catalog()?;
        let mut prepared_catalog_refs = prepared_catalogs.iter().collect::<Vec<_>>();
        prepared_catalog_refs.push(&first_party);
        let bundles = Arc::new(BundleCatalog::from_verified_catalogs(
            &prepared_catalog_refs,
        )?);
        let static_sources = static_bundle_skill_sources(&bundles)?;
        let agent_catalog = Arc::new(AgentCatalog::new(Arc::clone(&bundles))?);
        runtime.refresh(|candidate| {
            candidate.replace_catalog(Arc::clone(&agent_catalog));
            candidate.replace_sources_of_kind(RuntimeSourceKind::Bundle, static_sources.clone())
        })?;
        // Advance even when rows were skipped, so the warning is reported once
        // per generation instead of on every root binding.
        *applied_generation = snapshot.generation;
        Ok(true)
    }

    /// Decode one registry row, or explain why it is unreadable.
    fn decode_installed(record: &BundleRegistryRecord) -> Result<PreparedCatalog, String> {
        let prepared = PreparedCatalog::decode(&record.prepared_bytes, &record.prepared_digest)
            .map_err(|error| error.to_string())?;
        let [bundle] = prepared.bundles() else {
            return Err("prepared catalog must contain exactly one bundle".to_string());
        };
        let identity = bundle.identity();
        if identity.id != record.bundle_id
            || identity.version != record.version
            || identity.publisher != record.publisher
        {
            return Err("registry metadata does not match the prepared catalog".to_string());
        }
        Ok(prepared)
    }
}

/// Adapt every prepared bundle Skill through the shared contribution seam.
pub(crate) fn static_bundle_skill_sources(
    catalog: &BundleCatalog,
) -> Result<Vec<RuntimeSource>, CoreError> {
    let mut sources = Vec::new();
    for bundle in catalog.bundles() {
        let resources = bundle.skills();
        if resources.is_empty() {
            continue;
        }
        let contributions = PluginContributionSet {
            skills: resources
                .iter()
                .map(|resource| SkillContribution {
                    id: resource.local_id.clone(),
                    content: resource.content.clone(),
                    digest: resource.digest.clone(),
                })
                .collect(),
            ..PluginContributionSet::default()
        };
        let source =
            prepared_static_bundle_source(&bundle.identity().id, resources, &contributions)
                .map_err(|error| {
                    CoreError::Invalid(format!(
                        "prepare static Skills for bundle `{}`: {error}",
                        bundle.identity().id
                    ))
                })?;
        sources.push(source.into_runtime_source());
    }
    Ok(sources)
}

#[async_trait]
impl RuntimeCatalogRefresh for InstalledBundleRefresh {
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        InstalledBundleRefresh::refresh_if_changed(self, runtime).await
    }
}
