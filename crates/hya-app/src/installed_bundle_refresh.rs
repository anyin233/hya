use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use hya_bundle::{BundleCatalog, PreparedBundleSchemas, PreparedCatalog};
use hya_core::{
    AgentCatalog, CoreError, RuntimeCatalogRefresh, RuntimeRegistry, RuntimeSource,
    RuntimeSourceKind,
};
use hya_plugin::{PluginContributionSet, SkillContribution};
use hya_store::{BundleRegistry, BundleRegistryRecord};
use sha2::Digest as _;
use tokio::sync::{Mutex, OnceCell};

use crate::bundle_config::BundleConfigResolver;
use crate::project_bundles::load_project_bundle_dirs;
use crate::runtime_reconcile::{bundle_schema_claims, prepared_static_bundle_source};

/// First-party WorkflowBundle and AgentSetBundle payloads published with the runtime catalog.
const FIRST_PARTY_CATALOG_BUNDLES: [&str; 3] =
    ["hya/goal-loop", "hya/plan-impl-review", "hya/subagents"];

/// Load every first-party runtime bundle, in deterministic order.
pub fn first_party_catalogs() -> Result<Vec<PreparedCatalog>, CoreError> {
    FIRST_PARTY_CATALOG_BUNDLES
        .iter()
        .map(|identity| {
            let loaded = hya_bundle::first_party_bundle(identity)
                .map_err(|error| CoreError::Invalid(error.to_string()))?;
            PreparedCatalog::decode(loaded.bytes(), loaded.digest())
                .map_err(|error| CoreError::Invalid(format!("first-party `{identity}`: {error}")))
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
    config_file: PathBuf,
    /// `config.yml` files of the spawning bundles last published, with the
    /// content digest each runtime source was started with.
    watched_configs: Mutex<Vec<(PathBuf, Option<[u8; 32]>)>>,
    initialized: AtomicBool,
    sources: Mutex<BTreeMap<String, crate::bundle_runtime::CachedBundleSource>>,
    /// Read-only host services behind bundle process capabilities.
    host_reads: Option<Arc<dyn hya_core::HostSessionReads>>,
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
            config_file: crate::config::active_config_path(),
            watched_configs: Mutex::new(Vec::new()),
            initialized: AtomicBool::new(false),
            sources: Mutex::new(BTreeMap::new()),
            host_reads: None,
        }
    }

    /// Back bundle process capabilities (`session.usage`, for tool calls and
    /// bundle API requests) with these read-only host services.
    #[must_use]
    pub fn with_host_reads(mut self, reads: Arc<dyn hya_core::HostSessionReads>) -> Self {
        self.host_reads = Some(reads);
        self
    }

    /// Track a project bundle directory (highest-precedence catalog tier).
    #[must_use]
    pub fn with_project_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.project_dir = dir;
        self
    }

    /// Resolve user-scope bundle configuration beside `config_file` (the Hya
    /// `config.yaml`, which need not exist) instead of the active one.
    #[must_use]
    pub fn with_config_file(mut self, config_file: PathBuf) -> Self {
        self.config_file = config_file;
        self
    }

    /// Publish a new installed catalog generation when the registry advanced,
    /// a project bundle changed, or a spawning bundle's `config.yml` changed.
    ///
    /// Returns `Ok(true)` if the runtime registry was updated, `Ok(false)` when
    /// nothing changed.
    pub async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        let (project_bundles, project_fingerprint) = match &self.project_dir {
            Some(dir) => load_project_bundle_dirs(dir),
            None => (Vec::new(), 0),
        };
        let mut project_dirs = BTreeMap::new();
        let mut project_catalogs = Vec::with_capacity(project_bundles.len());
        for (catalog, dir) in project_bundles {
            if let [bundle] = catalog.bundles() {
                project_dirs
                    .entry(bundle.identity().id.clone())
                    .or_insert(dir);
            }
            project_catalogs.push(catalog);
        }
        let config_resolver = BundleConfigResolver::new(self.config_file.clone(), project_dirs);
        let mut watched_configs = self.watched_configs.lock().await;
        let mut applied_project_fingerprint = self.applied_project_fingerprint.lock().await;
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
        let registry_changed = !self.initialized.load(Ordering::Acquire)
            || registry_generation.unwrap_or(0) != *applied_generation;
        let project_changed = project_fingerprint != *applied_project_fingerprint;
        let config_changed = watched_configs.iter().any(|(file, digest)| {
            std::fs::read(file)
                .ok()
                .map(|bytes| <[u8; 32]>::from(sha2::Sha256::digest(bytes)))
                != *digest
        });
        if !registry_changed && !project_changed && !config_changed {
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
        let mut first_party = first_party_catalogs()?;
        let higher_ids = prepared_catalogs
            .iter()
            .chain(project_catalogs.iter())
            .filter_map(|catalog| catalog.bundles().first())
            .map(|bundle| bundle.identity().id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let higher_namespaces = prepared_catalogs
            .iter()
            .chain(project_catalogs.iter())
            .filter_map(|catalog| catalog.bundles().first())
            .map(|bundle| bundle.namespace())
            .collect::<std::collections::BTreeSet<_>>();
        first_party.retain(|catalog| {
            catalog.bundles().first().is_some_and(|bundle| {
                !higher_ids.contains(bundle.identity().id.as_str())
                    && !higher_namespaces.contains(bundle.namespace())
            })
        });
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
        let mut source_cache = self.sources.lock().await;
        let mut next_sources = BTreeMap::new();
        let mut next_watched = Vec::new();
        for bundle in bundles.bundles() {
            let id = &bundle.identity().id;
            let process = prepared_catalog_refs
                .iter()
                .find_map(|catalog| catalog.bundle_process(id));
            let schemas = schema_rows
                .iter()
                .find(|row| &row.bundle_id == id)
                .map_or(&[][..], |row| row.schemas.as_slice());
            let apis = prepared_catalog_refs
                .iter()
                .find(|catalog| {
                    catalog
                        .bundles()
                        .iter()
                        .any(|candidate| &candidate.identity().id == id)
                })
                .map_or(&[][..], |catalog| catalog.bundle_apis(id));
            let permission_modes = prepared_catalog_refs
                .iter()
                .find(|catalog| {
                    catalog
                        .bundles()
                        .iter()
                        .any(|candidate| &candidate.identity().id == id)
                })
                .map_or(&[][..], |catalog| catalog.bundle_permission_modes(id));
            let location = config_resolver.location(id).map_err(|error| {
                CoreError::Invalid(format!("resolve bundle `{id}` configuration: {error}"))
            })?;
            let config =
                crate::bundle_runtime::BundleRuntimeConfig::capture(bundle, process, location);
            if config.watched() {
                next_watched.push((config.location().file().to_path_buf(), config.digest()));
            }
            let fingerprint = crate::bundle_runtime::fingerprint(
                bundle,
                process,
                schemas,
                apis,
                permission_modes,
                &config,
            )?;
            let prepared = match source_cache.get(id) {
                Some(cached) if cached.fingerprint == fingerprint => cached.clone(),
                _ => {
                    crate::bundle_runtime::prepare_source(
                        bundle,
                        crate::bundle_runtime::BundleRuntimeParts {
                            process,
                            schemas,
                            apis,
                            permission_modes,
                            reads: self.host_reads.clone(),
                        },
                        &config,
                    )
                    .await?
                }
            };
            next_sources.insert(id.clone(), prepared);
        }
        let static_sources = next_sources
            .values()
            .map(|entry| entry.source.clone())
            .collect::<Vec<_>>();
        let agent_catalog = Arc::new(AgentCatalog::new(Arc::clone(&bundles))?);
        runtime.refresh(|candidate| {
            candidate.replace_catalog(Arc::clone(&agent_catalog));
            candidate.replace_sources_of_kind(RuntimeSourceKind::Bundle, static_sources.clone())
        })?;
        *source_cache = next_sources;
        *watched_configs = next_watched;
        self.initialized.store(true, Ordering::Release);
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
