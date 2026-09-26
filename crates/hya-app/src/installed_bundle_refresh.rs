use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use hya_bundle::{BundleCatalog, PreparedBundleSchemas, PreparedCatalog};
use hya_core::{
    AgentCatalog, CoreError, RuntimeCatalogRefresh, RuntimeRegistry, RuntimeSource,
    RuntimeSourceKind, ScopeKey,
};
use hya_plugin::{PluginContributionSet, SkillContribution};
use hya_store::{BundleRegistry, BundleRegistryRecord};
use sha2::Digest as _;
use tokio::sync::{Mutex, OnceCell};

use crate::bundle_config::BundleConfigResolver;
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

/// One runtime source cache key: the bundle id and its runtime fingerprint
/// (see `bundle_runtime::fingerprint`). Two scopes that compose the same
/// bundle with the same configuration location share one source (and one
/// process); a project bundle's location is its own source directory, so its
/// sources are per Project.
pub(crate) type SourceKey = (String, [u8; 32]);

/// Bundle runtime sources shared by the base catalog and every scope overlay.
///
/// Every entry is referenced by the base publication or by at least one live
/// scope's last publication; [`Self::collect`] drops the rest, which stops
/// their processes once no binding retains them either.
#[derive(Default)]
pub(crate) struct SourceCache {
    state: std::sync::Mutex<SourceCacheState>,
}

#[derive(Default)]
struct SourceCacheState {
    entries: HashMap<SourceKey, RuntimeSource>,
    base: BTreeSet<SourceKey>,
    scopes: HashMap<ScopeKey, BTreeSet<SourceKey>>,
}

impl SourceCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, SourceCacheState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The cached source for `key`, if any.
    pub(crate) fn get(&self, key: &SourceKey) -> Option<RuntimeSource> {
        self.lock().entries.get(key).cloned()
    }

    /// Record the sources the base catalog now publishes, then collect.
    pub(crate) fn commit_base(
        &self,
        sources: BTreeMap<SourceKey, RuntimeSource>,
        live: &[ScopeKey],
    ) {
        let dropped = {
            let mut state = self.lock();
            state.base = sources.keys().cloned().collect();
            for (key, source) in sources {
                state.entries.entry(key).or_insert(source);
            }
            state.collect(live)
        };
        drop(dropped);
    }

    /// Record the sources scope `scope` now publishes, then collect.
    pub(crate) fn commit_scope(
        &self,
        scope: ScopeKey,
        sources: BTreeMap<SourceKey, RuntimeSource>,
        live: &[ScopeKey],
    ) {
        let dropped = {
            let mut state = self.lock();
            state
                .scopes
                .insert(scope, sources.keys().cloned().collect());
            for (key, source) in sources {
                state.entries.entry(key).or_insert(source);
            }
            state.collect(live)
        };
        drop(dropped);
    }

    /// Forget the scopes that are not `live` and drop every source neither
    /// the base nor a live scope uses. Dropped sources stop once no binding
    /// retains them.
    pub(crate) fn collect(&self, live: &[ScopeKey]) {
        let dropped = self.lock().collect(live);
        // Stop released processes outside the lock.
        drop(dropped);
    }
}

impl SourceCacheState {
    fn collect(&mut self, live: &[ScopeKey]) -> Vec<RuntimeSource> {
        self.scopes.retain(|key, _| live.contains(key));
        let used = self
            .base
            .iter()
            .chain(self.scopes.values().flatten())
            .cloned()
            .collect::<BTreeSet<_>>();
        let unused = self
            .entries
            .keys()
            .filter(|key| !used.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        unused
            .into_iter()
            .filter_map(|key| self.entries.remove(&key))
            .collect()
    }
}

/// Installed and first-party catalogs of the last base publication; scope
/// overlays compose their project bundles on top of these.
pub(crate) struct BaseCatalogs {
    /// Advances with every base publication (0 = never published).
    pub(crate) revision: u64,
    /// Readable installed registry rows, in registry order.
    pub(crate) installed: Vec<Arc<PreparedCatalog>>,
    /// Every first-party catalog, before any shadowing.
    pub(crate) first_party: Vec<Arc<PreparedCatalog>>,
}

/// One composed catalog tier: the Bundle catalog, every Bundle-kind runtime
/// source keyed for the [`SourceCache`], and the `config.yml` files of the
/// spawning bundles with the digest each source started with.
pub(crate) struct Composition {
    pub(crate) bundles: Arc<BundleCatalog>,
    pub(crate) sources: BTreeMap<SourceKey, RuntimeSource>,
    pub(crate) watched: Vec<(PathBuf, Option<[u8; 32]>)>,
}

impl Composition {
    /// The runtime sources in bundle id order.
    pub(crate) fn runtime_sources(&self) -> Vec<RuntimeSource> {
        self.sources.values().cloned().collect()
    }
}

/// Compose project, installed, and first-party catalogs with the shadowing
/// rules and prepare (or reuse from `cache`) each bundle's runtime source.
///
/// Project bundles win: an installed row is skipped when a project bundle
/// claims the same identity id OR the same namespace. A first-party bundle is
/// skipped when any installed or project bundle claims its id or namespace.
/// Newly prepared sources are returned, not cached: the caller commits them
/// only after a successful publication.
pub(crate) async fn compose(
    project: &[&PreparedCatalog],
    installed: &[&PreparedCatalog],
    first_party: &[&PreparedCatalog],
    resolver: &BundleConfigResolver,
    cache: &SourceCache,
    reads: Option<&Arc<dyn hya_core::HostSessionReads>>,
) -> Result<Composition, CoreError> {
    let single = |catalog: &&PreparedCatalog| catalog.bundles().first().cloned();
    let project_bundles = project.iter().filter_map(single).collect::<Vec<_>>();
    let mut prepared_catalog_refs: Vec<&PreparedCatalog> = Vec::new();
    for catalog in installed {
        let Some(bundle) = catalog.bundles().first() else {
            continue;
        };
        let id = &bundle.identity().id;
        let namespace = bundle.namespace();
        let shadowed = project_bundles
            .iter()
            .any(|project| &project.identity().id == id || project.namespace() == namespace);
        if shadowed {
            tracing::warn!(
                bundle_id = %id,
                version = %bundle.identity().version,
                namespace = %namespace,
                "project bundle shadows installed bundle; uninstall it or remove the project directory entry"
            );
            continue;
        }
        prepared_catalog_refs.push(catalog);
    }
    prepared_catalog_refs.extend(project.iter().copied());
    let higher_ids = prepared_catalog_refs
        .iter()
        .filter_map(|catalog| catalog.bundles().first())
        .map(|bundle| bundle.identity().id.as_str())
        .collect::<BTreeSet<_>>();
    let higher_namespaces = prepared_catalog_refs
        .iter()
        .filter_map(|catalog| catalog.bundles().first())
        .map(|bundle| bundle.namespace())
        .collect::<BTreeSet<_>>();
    let first_party = first_party
        .iter()
        .copied()
        .filter(|catalog| {
            catalog.bundles().first().is_some_and(|bundle| {
                !higher_ids.contains(bundle.identity().id.as_str())
                    && !higher_namespaces.contains(bundle.namespace())
            })
        })
        .collect::<Vec<_>>();
    prepared_catalog_refs.extend(first_party);
    let bundles = Arc::new(BundleCatalog::from_verified_catalogs(
        &prepared_catalog_refs,
    )?);
    let mut schema_rows = Vec::new();
    for catalog in &prepared_catalog_refs {
        schema_rows.extend(catalog.schemas().iter().cloned());
    }
    let mut sources = BTreeMap::new();
    let mut watched = Vec::new();
    for bundle in bundles.bundles() {
        let id = &bundle.identity().id;
        let process = prepared_catalog_refs
            .iter()
            .find_map(|catalog| catalog.bundle_process(id));
        let schemas = schema_rows
            .iter()
            .find(|row| &row.bundle_id == id)
            .map_or(&[][..], |row| row.schemas.as_slice());
        let owner = prepared_catalog_refs.iter().find(|catalog| {
            catalog
                .bundles()
                .iter()
                .any(|candidate| &candidate.identity().id == id)
        });
        let apis = owner.map_or(&[][..], |catalog| catalog.bundle_apis(id));
        let permission_modes = owner.map_or(&[][..], |catalog| catalog.bundle_permission_modes(id));
        let location = resolver.location(id).map_err(|error| {
            CoreError::Invalid(format!("resolve bundle `{id}` configuration: {error}"))
        })?;
        let config = crate::bundle_runtime::BundleRuntimeConfig::capture(bundle, process, location);
        if config.watched() {
            watched.push((config.location().file().to_path_buf(), config.digest()));
        }
        let fingerprint = crate::bundle_runtime::fingerprint(
            bundle,
            process,
            schemas,
            apis,
            permission_modes,
            &config,
        )?;
        let key = (id.clone(), fingerprint);
        let source = match cache.get(&key) {
            Some(source) => source,
            None => {
                let prepared = crate::bundle_runtime::prepare_source(
                    bundle,
                    crate::bundle_runtime::BundleRuntimeParts {
                        process,
                        schemas,
                        apis,
                        permission_modes,
                        reads: reads.cloned(),
                    },
                    &config,
                )
                .await?;
                debug_assert_eq!(prepared.fingerprint, fingerprint);
                prepared.source
            }
        };
        sources.insert(key, source);
    }
    Ok(Composition {
        bundles,
        sources,
        watched,
    })
}

/// Lazily publishes installed Bundle catalog changes at root binding boundaries.
///
/// This is the base tier every scope shares: installed registry bundles plus
/// the first-party bundles. Project bundles are per registered Project and
/// compose on top of it in [`crate::ProjectScopeRefresh`].
pub struct InstalledBundleRefresh {
    registry_path: PathBuf,
    registry: OnceCell<BundleRegistry>,
    applied_generation: Mutex<u64>,
    config_file: PathBuf,
    /// `config.yml` files of the spawning bundles last published, with the
    /// content digest each runtime source was started with.
    watched_configs: Mutex<Vec<(PathBuf, Option<[u8; 32]>)>>,
    initialized: AtomicBool,
    sources: Arc<SourceCache>,
    base: std::sync::Mutex<Arc<BaseCatalogs>>,
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
            config_file: crate::config::active_config_path(),
            watched_configs: Mutex::new(Vec::new()),
            initialized: AtomicBool::new(false),
            sources: Arc::new(SourceCache::default()),
            base: std::sync::Mutex::new(Arc::new(BaseCatalogs {
                revision: 0,
                installed: Vec::new(),
                first_party: Vec::new(),
            })),
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

    /// Resolve user-scope bundle configuration beside `config_file` (the Hya
    /// `config.yaml`, which need not exist) instead of the active one.
    #[must_use]
    pub fn with_config_file(mut self, config_file: PathBuf) -> Self {
        self.config_file = config_file;
        self
    }

    /// The Hya `config.yaml` user-scope bundle configuration resolves beside.
    pub(crate) fn config_file(&self) -> &std::path::Path {
        &self.config_file
    }

    /// The runtime source cache shared with scope overlays.
    pub(crate) fn source_cache(&self) -> &Arc<SourceCache> {
        &self.sources
    }

    /// Read-only host services for bundle processes.
    pub(crate) fn host_reads(&self) -> Option<&Arc<dyn hya_core::HostSessionReads>> {
        self.host_reads.as_ref()
    }

    /// Installed and first-party catalogs of the last base publication.
    pub(crate) fn base_catalogs(&self) -> Arc<BaseCatalogs> {
        Arc::clone(
            &self
                .base
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Publish a new installed catalog generation when the registry advanced
    /// or a spawning bundle's `config.yml` changed.
    ///
    /// Returns `Ok(true)` if the runtime registry was updated, `Ok(false)` when
    /// nothing changed.
    pub async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        let config_resolver = BundleConfigResolver::new(self.config_file.clone(), BTreeMap::new());
        let mut watched_configs = self.watched_configs.lock().await;
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
        let config_changed = watched_configs.iter().any(|(file, digest)| {
            std::fs::read(file)
                .ok()
                .map(|bytes| <[u8; 32]>::from(sha2::Sha256::digest(bytes)))
                != *digest
        });
        if !registry_changed && !config_changed {
            return Ok(false);
        }

        let snapshot = match &self.registry.get() {
            Some(registry) => Some(registry.snapshot().await?),
            None => None,
        };
        // A row written by a different binary version cannot decode. Skip it
        // with a named warning and keep the rest of the catalog usable: a single
        // stale row must not wedge every later turn, and the operator needs to
        // know which bundle to reinstall.
        let mut installed = Vec::with_capacity(
            snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.bundles.len()),
        );
        if let Some(snapshot) = &snapshot {
            for record in &snapshot.bundles {
                match Self::decode_installed(record) {
                    Ok(prepared) => installed.push(Arc::new(prepared)),
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
        }
        let first_party = first_party_catalogs()?
            .into_iter()
            .map(Arc::new)
            .collect::<Vec<_>>();
        let installed_refs = installed.iter().map(Arc::as_ref).collect::<Vec<_>>();
        let first_party_refs = first_party.iter().map(Arc::as_ref).collect::<Vec<_>>();
        let composition = compose(
            &[],
            &installed_refs,
            &first_party_refs,
            &config_resolver,
            &self.sources,
            self.host_reads.as_ref(),
        )
        .await?;
        let static_sources = composition.runtime_sources();
        let agent_catalog = Arc::new(AgentCatalog::new(Arc::clone(&composition.bundles))?);
        runtime.refresh(|candidate| {
            candidate.replace_catalog(Arc::clone(&agent_catalog));
            candidate.replace_sources_of_kind(RuntimeSourceKind::Bundle, static_sources.clone())
        })?;
        self.sources
            .commit_base(composition.sources, &runtime.scope_keys());
        *watched_configs = composition.watched;
        {
            let mut base = self
                .base
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *base = Arc::new(BaseCatalogs {
                revision: base.revision + 1,
                installed,
                first_party,
            });
        }
        self.initialized.store(true, Ordering::Release);
        // Advance even when rows were skipped, so the warning is reported once
        // per generation instead of on every root binding.
        if let Some(generation) = registry_generation {
            *applied_generation = generation;
        }
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
