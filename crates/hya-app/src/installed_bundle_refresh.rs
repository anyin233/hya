use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{BundleCatalog, PreparedBundleSchemas, PreparedCatalog};
use hya_core::{
    AgentCatalog, CoreError, RuntimeCatalogRefresh, RuntimeRegistry, RuntimeSource,
    RuntimeSourceKind,
};
use hya_plugin::{PluginContributionSet, SkillContribution};
use hya_store::{BundleRegistry, BundleRegistryRecord};
use tokio::sync::{Mutex, OnceCell};

use crate::project_bundles::load_project_bundles;
use crate::runtime_reconcile::{bundle_schema_claims, prepared_static_bundle_source};

/// One embedded first-party bundle entry emitted by the build script.
#[derive(serde::Deserialize)]
struct FirstPartyEntry {
    name: String,
    digest: String,
    bytes: String,
}

/// Decode every build-prepared first-party bundle, in deterministic order.
pub fn first_party_catalogs() -> Result<Vec<PreparedCatalog>, CoreError> {
    let raw = include_str!(concat!(env!("OUT_DIR"), "/first-party.json"));
    let entries: Vec<FirstPartyEntry> = serde_json::from_str(raw).map_err(|error| {
        CoreError::Invalid(format!(
            "embedded first-party catalog list is malformed: {error}"
        ))
    })?;
    entries
        .iter()
        .map(|entry| {
            PreparedCatalog::decode(entry.bytes.as_bytes(), &entry.digest).map_err(|error| {
                CoreError::Invalid(format!(
                    "embedded first-party bundle `{}` failed decode: {error}",
                    entry.name
                ))
            })
        })
        .collect()
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
    applied_project_fingerprint: Mutex<u64>,
    project_dir: Option<PathBuf>,
}

impl InstalledBundleRefresh {
    /// Track installed-catalog generations for `registry_path`.
    #[must_use]
    pub fn new(registry_path: PathBuf) -> Self {
        Self {
            registry_path,
            registry: OnceCell::new(),
            applied_generation: Mutex::new(0),
            applied_project_fingerprint: Mutex::new(0),
            project_dir: None,
        }
    }

    /// Track a project bundle directory (highest-precedence catalog tier).
    #[must_use]
    pub fn with_project_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.project_dir = dir;
        self
    }

    /// Publish a new installed catalog generation when the registry advanced.
    ///
    /// Returns `Ok(true)` if the runtime registry was updated, `Ok(false)` when
    /// the path is missing or the generation is unchanged.
    pub async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        let (project_catalogs, project_fingerprint) = match &self.project_dir {
            Some(dir) => load_project_bundles(dir),
            None => (Vec::new(), 0),
        };
        let mut applied_project_fingerprint = self.applied_project_fingerprint.lock().await;
        if self.registry.get().is_none()
            && !self.registry_path.try_exists().map_err(|error| {
                CoreError::Invalid(format!("inspect installed Bundle registry path: {error}"))
            })?
            && project_catalogs.is_empty()
        {
            return Ok(false);
        }
        let registry_generation = if self.registry.get().is_none()
            && !self.registry_path.try_exists().map_err(|error| {
                CoreError::Invalid(format!("inspect installed Bundle registry path: {error}"))
            })? {
            None
        } else {
            let path = self.registry_path.to_str().ok_or_else(|| {
                CoreError::Invalid("installed Bundle registry path is not UTF-8".to_string())
            })?;
            let registry = self
                .registry
                .get_or_try_init(|| async { BundleRegistry::connect(path).await })
                .await?;
            Some(registry.generation().await?)
        };
        let mut applied_generation = self.applied_generation.lock().await;
        let registry_changed =
            registry_generation.is_none_or(|generation| generation != *applied_generation);
        let project_changed = project_fingerprint != *applied_project_fingerprint;
        if !registry_changed && !project_changed {
            return Ok(false);
        }

        // Project bundles are the highest tier: an installed row is skipped
        // when a project bundle claims the same identity id OR the same
        // namespace, so scope precedence is a plain exclusion.
        let project_ids: Vec<String> = project_catalogs
            .iter()
            .map(|catalog| {
                let [bundle] = catalog.bundles() else {
                    return String::new();
                };
                bundle.identity().id.to_string()
            })
            .collect();
        let project_namespaces: Vec<String> = project_catalogs
            .iter()
            .map(|catalog| {
                let [bundle] = catalog.bundles() else {
                    return String::new();
                };
                bundle.namespace().to_string()
            })
            .collect();

        let snapshot = match &self.registry.get() {
            Some(registry) => Some(registry.snapshot().await?),
            None => None,
        };
        // A row written by a different binary version cannot decode. Skip it
        // with a named warning and keep the rest of the catalog usable: a single
        // stale row must not wedge every later turn, and the operator needs to
        // know which bundle to reinstall.
        let mut prepared_catalogs = Vec::with_capacity(
            snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.bundles.len())
                + project_catalogs.len(),
        );
        if let Some(snapshot) = &snapshot {
            for record in &snapshot.bundles {
                let prepared = match Self::decode_installed(record) {
                    Ok(prepared) => prepared,
                    Err(detail) => {
                        tracing::warn!(
                            bundle_id = %record.bundle_id,
                            version = %record.version,
                            error = %detail,
                            "skipping unreadable installed bundle; reinstall it with `hya bundle install`"
                        );
                        continue;
                    }
                };
                let [bundle] = prepared.bundles() else {
                    continue;
                };
                let record_namespace = bundle.namespace().to_string();
                let shadowed = project_ids.contains(&record.bundle_id)
                    || (project_namespaces.contains(&record_namespace)
                        && project_catalogs.iter().any(|catalog| {
                            let [project_bundle] = catalog.bundles() else {
                                return false;
                            };
                            project_bundle.identity().id != record.bundle_id
                                && project_bundle.namespace() == record_namespace
                        }));
                if shadowed {
                    tracing::warn!(
                        bundle_id = %record.bundle_id,
                        version = %record.version,
                        namespace = %record_namespace,
                        "project bundle shadows installed bundle; uninstall it or remove the project directory entry"
                    );
                    continue;
                }
                prepared_catalogs.push(prepared);
            }
        }
        let first_party = first_party_catalogs()?;
        let mut prepared_catalog_refs = prepared_catalogs.iter().collect::<Vec<_>>();
        prepared_catalog_refs.extend(project_catalogs.iter());
        prepared_catalog_refs.extend(first_party.iter());
        let bundles = Arc::new(BundleCatalog::from_verified_catalogs(
            &prepared_catalog_refs,
        )?);
        let mut schema_rows = Vec::new();
        for catalog in &prepared_catalog_refs {
            schema_rows.extend(catalog.schemas().iter().cloned());
        }
        let static_sources = static_bundle_skill_sources(&bundles, &schema_rows)?;
        let agent_catalog = Arc::new(AgentCatalog::new(Arc::clone(&bundles))?);
        runtime.refresh(|candidate| {
            candidate.replace_catalog(Arc::clone(&agent_catalog));
            candidate.replace_sources_of_kind(RuntimeSourceKind::Bundle, static_sources.clone())
        })?;
        // Advance even when rows were skipped, so the warning is reported once
        // per generation instead of on every root binding.
        if let Some(generation) = registry_generation {
            *applied_generation = generation;
        }
        *applied_project_fingerprint = project_fingerprint;
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

/// Adapt every prepared bundle Skill through the shared contribution seam and
/// attach the bundle's declared schema extensions as scheme claims.
pub(crate) fn static_bundle_skill_sources(
    catalog: &BundleCatalog,
    schema_rows: &[PreparedBundleSchemas],
) -> Result<Vec<RuntimeSource>, CoreError> {
    let mut sources = Vec::new();
    for bundle in catalog.bundles() {
        let resources = bundle.skills();
        let bundle_id = bundle.identity().id.as_str();
        let schemas = schema_rows
            .iter()
            .find(|row| row.bundle_id == bundle_id)
            .map(|row| row.schemas.as_slice())
            .unwrap_or_default();
        if resources.is_empty() && schemas.is_empty() {
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
        let claims = bundle_schema_claims(bundle_id, schemas, bundle.tools()).map_err(|error| {
            CoreError::Invalid(format!(
                "resolve scheme claims for bundle `{bundle_id}`: {error}"
            ))
        })?;
        let source = prepared_static_bundle_source(bundle_id, resources, &contributions).map_err(
            |error| {
                CoreError::Invalid(format!(
                    "prepare static Skills for bundle `{}`: {error}",
                    bundle.identity().id
                ))
            },
        )?;
        sources.push(source.with_schemas(claims).into_runtime_source());
    }
    Ok(sources)
}

#[async_trait]
impl RuntimeCatalogRefresh for InstalledBundleRefresh {
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        InstalledBundleRefresh::refresh_if_changed(self, runtime).await
    }
}
