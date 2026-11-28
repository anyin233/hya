use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::bundle_registry::BundleRegistry;
use async_trait::async_trait;
use hya_bundle::{
    Bundle, BundleCatalog, BundleError, PreparedBundleSchemas, PreparedCatalog,
    missing_prerequisites,
};
use hya_engine::{
    CoreError, RuntimeCatalogRefresh, RuntimeRegistry, RuntimeSource, RuntimeSourceKind, ScopeKey,
};
use hya_plugin::{PluginContributionSet, SkillContribution};
use sha2::Digest as _;
use tokio::sync::{Mutex, OnceCell};

use crate::bundle_config::BundleConfigResolver;
use crate::bundle_runtime::source::{bundle_schema_claims, prepared_static_bundle_source};

/// First-party bundles published with the runtime catalog, including workflows,
/// `hya/coding-extras`, `hya/multi-agent`, `hya/agent-channels`, `hya/plan`,
/// `hya/goal`, `hya/loop`, the TUI extension of the built-in panes, and
/// `hya/workspace-files`.
const FIRST_PARTY_CATALOG_BUNDLES: [&str; 9] = [
    "hya/coding-extras",
    "hya/multi-agent",
    "hya/agent-channels",
    "hya/plan",
    "hya/goal",
    "hya/loop",
    "hya/workflow",
    "hya/basic-tui-components",
    "hya/workspace-files",
];

/// Load every enabled, present first-party runtime bundle using the active
/// `config.yaml` [`hya_bundle::BuiltinBundleSelection`].
pub fn first_party_catalogs() -> Result<Vec<PreparedCatalog>, CoreError> {
    first_party_catalogs_for(&crate::config::configured_builtin_bundles())
}

/// Load every enabled, present first-party runtime bundle for `selection` in
/// deterministic order, skipping disabled or missing built-ins without panicking.
pub fn first_party_catalogs_for(
    selection: &hya_bundle::BuiltinBundleSelection,
) -> Result<Vec<PreparedCatalog>, CoreError> {
    let mut catalogs = Vec::new();
    for identity in FIRST_PARTY_CATALOG_BUNDLES {
        if !selection.is_enabled(identity) {
            continue;
        }
        let Some(loaded) = hya_bundle::first_party_bundle_optional(identity)
            .map_err(|error| CoreError::Invalid(error.to_string()))?
        else {
            continue;
        };
        catalogs.push(
            PreparedCatalog::decode(loaded.bytes(), loaded.digest()).map_err(|error| {
                CoreError::Invalid(format!("first-party `{identity}`: {error}"))
            })?,
        );
    }
    Ok(catalogs)
}

/// One `(bundle_id, warning)` per bundle left out for a missing prerequisite.
pub(crate) type Unsatisfied = Vec<(String, String)>;

/// Check the `requires` of a composed bundle set: every first-party bundle
/// `selection` enables (catalog or not) plus the `third_party` (installed or
/// project) catalogs, before shadowing. Returns the third-party catalogs whose
/// prerequisites are met, in input order, and one `(bundle_id, warning)` per
/// catalog left out.
///
/// # Errors
/// [`BundleError::MissingPrerequisite`] when an enabled first-party bundle
/// misses one (a configuration error), or a first-party load failure.
pub(crate) fn retain_satisfied<'a>(
    selection: &hya_bundle::BuiltinBundleSelection,
    third_party: Vec<&'a PreparedCatalog>,
) -> Result<(Vec<&'a PreparedCatalog>, Unsatisfied), BundleError> {
    let mut first_party = Vec::new();
    for identity in selection.enabled_identities() {
        if let Some(catalog) = hya_bundle::first_party_bundle_optional(identity)? {
            first_party.push(&catalog.bundles()[0]);
        }
    }
    retain_satisfied_with(&first_party, third_party)
}

fn retain_satisfied_with<'a>(
    first_party: &[&Bundle],
    mut third_party: Vec<&'a PreparedCatalog>,
) -> Result<(Vec<&'a PreparedCatalog>, Unsatisfied), BundleError> {
    let mut warnings = Vec::new();
    // Dropping a bundle can unsatisfy another, so repeat until none is dropped.
    loop {
        let replaced = third_party
            .iter()
            .copied()
            .flat_map(|catalog| {
                let bundle = &catalog.bundles()[0];
                [Some(bundle.identity().id.as_str()), bundle.overrides()]
            })
            .flatten()
            .collect::<BTreeSet<_>>();
        let first_party = first_party
            .iter()
            .copied()
            .filter(|bundle| !replaced.contains(bundle.identity().id.as_str()))
            .collect::<Vec<_>>();
        let present = first_party
            .iter()
            .map(|bundle| bundle.identity().id.as_str())
            .chain(replaced.iter().copied())
            .collect::<BTreeSet<_>>();
        if let Some((bundle_id, requires)) = missing_prerequisites(&first_party, &present).first() {
            return Err(BundleError::MissingPrerequisite {
                bundle_id: (*bundle_id).to_string(),
                requires: (*requires).to_string(),
            });
        }
        let before = third_party.len();
        third_party.retain(|catalog| {
            let missing = missing_prerequisites(&[&catalog.bundles()[0]], &present);
            let Some((bundle_id, requires)) = missing.first() else {
                return true;
            };
            warnings.push((
                (*bundle_id).to_string(),
                format!(
                    "bundle `{bundle_id}` requires bundle `{requires}`, which is not enabled; `{bundle_id}` is not loaded"
                ),
            ));
            false
        });
        if third_party.len() == before {
            return Ok((third_party, warnings));
        }
    }
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
    /// Bundle ids disabled in every scope.
    pub(crate) disabled: BTreeSet<String>,
    /// Every first-party catalog, before any shadowing.
    pub(crate) first_party: Vec<Arc<PreparedCatalog>>,
    /// Rows skipped at the last publication, also reported by explicit refresh.
    unreadable_errors: Vec<(String, String)>,
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
    disabled: &BTreeSet<String>,
    resolver: &BundleConfigResolver,
    cache: &SourceCache,
    reads: Option<&Arc<dyn hya_engine::HostSessionReads>>,
) -> Result<Composition, CoreError> {
    let single = |catalog: &&PreparedCatalog| {
        catalog
            .bundles()
            .first()
            .filter(|bundle| !disabled.contains(&bundle.identity().id))
            .cloned()
    };
    let project_bundles = project.iter().filter_map(single).collect::<Vec<_>>();
    let mut prepared_catalog_refs: Vec<&PreparedCatalog> = Vec::new();
    for catalog in installed {
        let Some(bundle) = catalog.bundles().first() else {
            continue;
        };
        if disabled.contains(&bundle.identity().id) {
            continue;
        }
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
    prepared_catalog_refs.extend(project.iter().copied().filter(|catalog| {
        catalog
            .bundles()
            .first()
            .is_some_and(|bundle| !disabled.contains(&bundle.identity().id))
    }));
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
    let higher_overrides = prepared_catalog_refs
        .iter()
        .filter_map(|catalog| catalog.bundles().first())
        .filter_map(|bundle| bundle.overrides())
        .collect::<BTreeSet<_>>();
    let first_party = first_party
        .iter()
        .copied()
        .filter(|catalog| {
            catalog.bundles().first().is_some_and(|bundle| {
                !disabled.contains(&bundle.identity().id)
                    && !higher_ids.contains(bundle.identity().id.as_str())
                    && !higher_namespaces.contains(bundle.namespace())
                    && !higher_overrides.contains(bundle.identity().id.as_str())
            })
        })
        .collect::<Vec<_>>();
    prepared_catalog_refs.extend(first_party);
    let bundles = Arc::new(
        BundleCatalog::from_verified_catalogs(&prepared_catalog_refs).map_err(CoreError::bundle)?,
    );
    let mut schema_rows = Vec::new();
    for catalog in &prepared_catalog_refs {
        schema_rows.extend(catalog.schemas().iter().cloned());
    }
    let mut sources = BTreeMap::new();
    let mut watched = Vec::new();
    for bundle in bundles.bundles() {
        let id = &bundle.identity().id;
        let attribute = |error: CoreError| CoreError::BundleRuntime {
            bundle_id: id.clone(),
            source: Box::new(error),
        };
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
            attribute(CoreError::Invalid(format!(
                "resolve the bundle configuration: {error}"
            )))
        })?;
        let config = crate::bundle_runtime::BundleRuntimeConfig::capture(bundle, process, location);
        if config.watched() {
            watched.push((config.location().file().to_path_buf(), config.digest()));
        }
        let fingerprint = crate::bundle_runtime::fingerprint(bundle, &config).map_err(attribute)?;
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
                .await
                .map_err(attribute)?;
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
    io_tasks: Option<tokio_util::task::TaskTracker>,
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
    host_reads: Option<Arc<dyn hya_engine::HostSessionReads>>,
    /// Active built-in bundle selection from `config.yaml`.
    builtin_selection: hya_bundle::BuiltinBundleSelection,
}

impl InstalledBundleRefresh {
    /// Track installed-catalog generations for `registry_path`.
    #[must_use]
    pub fn new(registry_path: PathBuf) -> Self {
        Self {
            registry_path,
            io_tasks: None,
            registry: OnceCell::new(),
            applied_generation: Mutex::new(0),
            config_file: crate::config::active_config_path(),
            watched_configs: Mutex::new(Vec::new()),
            initialized: AtomicBool::new(false),
            sources: Arc::new(SourceCache::default()),
            builtin_selection: hya_bundle::BuiltinBundleSelection::full(),
            base: std::sync::Mutex::new(Arc::new(BaseCatalogs {
                revision: 0,
                installed: Vec::new(),
                disabled: BTreeSet::new(),
                first_party: Vec::new(),
                unreadable_errors: Vec::new(),
            })),
            host_reads: None,
        }
    }

    pub(crate) fn with_io_tasks(mut self, tasks: tokio_util::task::TaskTracker) -> Self {
        self.io_tasks = Some(tasks);
        self
    }

    pub(crate) fn io_tasks(&self) -> Option<tokio_util::task::TaskTracker> {
        self.io_tasks.clone()
    }

    /// Back bundle process capabilities (`session.usage`, for tool calls and
    /// bundle API requests) with these read-only host services.
    #[must_use]
    pub fn with_host_reads(mut self, reads: Arc<dyn hya_engine::HostSessionReads>) -> Self {
        self.host_reads = Some(reads);
        self
    }

    /// Filter first-party built-in bundles according to `config.yaml` `bundles.builtin`.
    #[must_use]
    pub fn with_builtin_selection(mut self, selection: hya_bundle::BuiltinBundleSelection) -> Self {
        self.builtin_selection = selection;
        self
    }

    /// Active built-in bundle selection governing first-party bundle loading.
    pub(crate) fn builtin_selection(&self) -> &hya_bundle::BuiltinBundleSelection {
        &self.builtin_selection
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
    pub(crate) fn host_reads(&self) -> Option<&Arc<dyn hya_engine::HostSessionReads>> {
        self.host_reads.as_ref()
    }

    /// The installed rows the last base publication skipped, as
    /// `installed bundle <id>: <error>` lines.
    pub(crate) fn diagnostics(&self) -> Vec<String> {
        self.base_catalogs()
            .unreadable_errors
            .iter()
            .map(|(id, error)| format!("installed bundle {id}: {error}"))
            .collect()
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

    /// Publish a new installed catalog generation when the registry advances.
    /// Bundle configuration files are inspected by explicit refresh.
    ///
    /// Returns `Ok(true)` if the runtime registry was updated, `Ok(false)` when
    /// nothing changed.
    pub async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        self.refresh(runtime, false).await
    }

    async fn refresh(
        &self,
        runtime: &RuntimeRegistry,
        inspect_configs: bool,
    ) -> Result<bool, CoreError> {
        hya_child_io::io_tasks::scope(
            self.io_tasks(),
            self.refresh_scoped(runtime, inspect_configs),
        )
        .await
    }

    async fn refresh_scoped(
        &self,
        runtime: &RuntimeRegistry,
        inspect_configs: bool,
    ) -> Result<bool, CoreError> {
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
                .await
                .map_err(CoreError::bundle)?;
            Some(registry.generation().await.map_err(CoreError::bundle)?)
        };
        let mut applied_generation = self.applied_generation.lock().await;
        let registry_changed = !self.initialized.load(Ordering::Acquire)
            || registry_generation.unwrap_or(0) != *applied_generation;
        let config_changed = inspect_configs
            && watched_configs.iter().any(|(file, digest)| {
                std::fs::read(file)
                    .ok()
                    .map(|bytes| <[u8; 32]>::from(sha2::Sha256::digest(bytes)))
                    != *digest
            });
        if !registry_changed && !config_changed {
            return Ok(false);
        }

        let snapshot = match &self.registry.get() {
            Some(registry) => Some(
                registry
                    .snapshot_with_catalogs()
                    .await
                    .map_err(CoreError::bundle)?,
            ),
            None => None,
        };
        let disabled = match &self.registry.get() {
            Some(registry) => registry
                .disabled_bundle_ids()
                .await
                .map_err(CoreError::bundle)?,
            None => BTreeSet::new(),
        };
        let unreadable_errors = snapshot.as_ref().map_or_else(Vec::new, |snapshot| {
            snapshot
                .unreadable
                .iter()
                .filter(|row| !disabled.contains(&row.bundle_id))
                .map(|row| (row.bundle_id.clone(), row.error.clone()))
                .collect::<Vec<_>>()
        });
        let installed = snapshot
            .into_iter()
            .flat_map(|snapshot| snapshot.bundles)
            .map(|loaded| Arc::new(loaded.prepared));
        let first_party = first_party_catalogs_for(&self.builtin_selection)?
            .into_iter()
            .map(Arc::new)
            .collect::<Vec<_>>();
        let previous = self.base_catalogs();
        let mut errors = unreadable_errors;
        let mut published = Vec::new();
        let mut sources = BTreeMap::new();
        let mut watched = Vec::new();
        for catalog in installed {
            let bundle = &catalog.bundles()[0];
            let id = &bundle.identity().id;
            if disabled.contains(id) {
                published.push(catalog);
                continue;
            }
            // Remember attempted config even on failure, so binds retry only
            // when input changes. Explicit refresh forces another attempt.
            let config = crate::bundle_runtime::BundleRuntimeConfig::capture(
                bundle,
                catalog.bundle_process(id),
                config_resolver
                    .location(id)
                    .map_err(|error| CoreError::Invalid(error.to_string()))?,
            );
            if config.watched() {
                watched.push((config.location().file().to_path_buf(), config.digest()));
            }
            match compose(
                &[],
                &[catalog.as_ref()],
                &[],
                &disabled,
                &config_resolver,
                &self.sources,
                self.host_reads.as_ref(),
            )
            .await
            {
                Ok(composition) => {
                    sources.extend(composition.sources);
                    published.push(catalog);
                }
                Err(error) => {
                    errors.push((id.clone(), error.to_string()));
                    if let Some(old) = previous
                        .installed
                        .iter()
                        .find(|old| old.bundles()[0].identity().id == *id)
                    {
                        published.push(Arc::clone(old));
                        let cache = self.sources.lock();
                        for key in cache.base.iter().filter(|key| key.0 == *id) {
                            if let Some(source) = cache.entries.get(key) {
                                sources.insert(key.clone(), source.clone());
                            }
                        }
                    }
                }
            }
        }
        let installed = published;
        let enabled = installed
            .iter()
            .map(Arc::as_ref)
            .filter(|catalog| !disabled.contains(&catalog.bundles()[0].identity().id))
            .collect::<Vec<_>>();
        let (enabled, unsatisfied) =
            retain_satisfied(&self.builtin_selection, enabled).map_err(CoreError::bundle)?;
        sources.retain(|(id, _), _| {
            enabled
                .iter()
                .any(|catalog| catalog.bundles()[0].identity().id == *id)
        });
        errors.extend(unsatisfied);
        let first_party_refs = first_party
            .iter()
            .filter(|catalog| {
                let bundle = &catalog.bundles()[0];
                !enabled.iter().any(|installed| {
                    let installed = &installed.bundles()[0];
                    installed.identity().id == bundle.identity().id
                        || installed.namespace() == bundle.namespace()
                        || installed.overrides() == Some(bundle.identity().id.as_str())
                })
            })
            .map(Arc::as_ref)
            .collect::<Vec<_>>();
        let first = compose(
            &[],
            &[],
            &first_party_refs,
            &disabled,
            &config_resolver,
            &self.sources,
            self.host_reads.as_ref(),
        )
        .await?;
        sources.extend(first.sources);
        watched.extend(first.watched);
        let mut catalogs = enabled;
        catalogs.extend(
            first_party_refs
                .into_iter()
                .filter(|catalog| !disabled.contains(&catalog.bundles()[0].identity().id)),
        );
        let composition = Composition {
            bundles: Arc::new(
                BundleCatalog::from_verified_catalogs(&catalogs).map_err(CoreError::bundle)?,
            ),
            sources,
            watched,
        };
        let static_sources = composition.runtime_sources();
        let agent_catalog = Arc::new(
            crate::runtime::agent_catalog_for_selection(
                Arc::clone(&composition.bundles),
                &self.builtin_selection,
            )
            .map_err(CoreError::bundle)?,
        );
        runtime
            .refresh(|candidate| {
                candidate.replace_catalog(Arc::clone(&agent_catalog));
                candidate.replace_sources_of_kind(RuntimeSourceKind::Bundle, static_sources.clone())
            })
            .map_err(CoreError::runtime_refresh)?;
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
                disabled,
                first_party,
                unreadable_errors: errors.clone(),
            });
        }
        self.initialized.store(true, Ordering::Release);
        // Advance even when rows were skipped, so the warning is reported once
        // per generation instead of on every root binding.
        if let Some(generation) = registry_generation {
            *applied_generation = generation;
        }
        if errors.is_empty() {
            Ok(true)
        } else {
            Err(CoreError::BundleRefresh(errors))
        }
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

    async fn refresh_explicit(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        // Retry failed sources even when their inputs have not changed.
        if !self.base_catalogs().unreadable_errors.is_empty() {
            self.initialized.store(false, Ordering::Release);
        }
        let changed = self.refresh(runtime, true).await?;
        let base = self.base_catalogs();
        if base.unreadable_errors.is_empty() {
            Ok(changed)
        } else {
            Err(CoreError::BundleRefresh(base.unreadable_errors.clone()))
        }
    }

    async fn diagnostics(&self, _scope: &hya_engine::CatalogScope) -> Vec<String> {
        InstalledBundleRefresh::diagnostics(self)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn catalog(id: &str, body: &str) -> PreparedCatalog {
        let manifest = format!(
            "kind: Bundle\nschema_version: 2\nidentity: {{ id: {id}, version: 1.0.0, publisher: acme }}\n{body}"
        );
        hya_bundle::prepare_package(hya_bundle::BundleSource::new(
            id,
            vec![hya_bundle::SourceFile::new("bundle.yaml", manifest)],
        ))
        .unwrap_or_else(|error| panic!("prepare {id}: {error}"))
    }

    fn ids(kept: &[&PreparedCatalog]) -> Vec<String> {
        kept.iter()
            .map(|catalog| catalog.bundles()[0].identity().id.clone())
            .collect()
    }

    #[test]
    fn a_first_party_bundle_missing_a_prerequisite_is_a_configuration_error() {
        let channels = catalog("acme/channels", "");
        let agents = catalog("acme/agents", "requires: [acme/channels]\n");
        let first_party = [&agents.bundles()[0]];
        assert_eq!(
            retain_satisfied_with(&first_party, Vec::new()).unwrap_err(),
            BundleError::MissingPrerequisite {
                bundle_id: "acme/agents".into(),
                requires: "acme/channels".into(),
            }
        );
        let message = retain_satisfied_with(&first_party, Vec::new())
            .unwrap_err()
            .to_string();
        assert!(message.contains("`acme/channels`"), "{message}");
        // An installed or project bundle may supply it.
        let (kept, warnings) = retain_satisfied_with(&first_party, vec![&channels]).unwrap();
        assert_eq!(ids(&kept), ["acme/channels"]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn third_party_bundles_missing_a_prerequisite_are_dropped_to_a_fixpoint() {
        let base = catalog("acme/base", "");
        let top = catalog("acme/top", "requires: [acme/middle]\n");
        let middle = catalog("acme/middle", "requires: [acme/absent]\n");
        let ok = catalog("acme/ok", "requires: [acme/base]\n");
        let first_party = [&base.bundles()[0]];
        let (kept, warnings) =
            retain_satisfied_with(&first_party, vec![&top, &ok, &middle]).unwrap();
        assert_eq!(ids(&kept), ["acme/ok"]);
        assert_eq!(
            warnings
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            ["acme/middle", "acme/top"]
        );
        assert!(warnings[0].1.contains("`acme/absent`"), "{warnings:?}");
    }

    #[test]
    fn overriders_satisfy_their_target_and_a_dropped_one_keeps_it() {
        let target = catalog("acme/target", "");
        let user = catalog("acme/user", "requires: [acme/target]\n");
        let replacement = catalog("acme/replacement", "overrides: acme/target\n");
        let broken = catalog(
            "acme/broken",
            "overrides: acme/target\nrequires: [acme/absent]\n",
        );
        // The target is not enabled; its overrider stands in for it.
        let (kept, _) = retain_satisfied_with(&[&user.bundles()[0]], vec![&replacement]).unwrap();
        assert_eq!(ids(&kept), ["acme/replacement"]);
        // A dropped overrider leaves the first-party target in the set.
        let first_party = [&target.bundles()[0], &user.bundles()[0]];
        let (kept, warnings) = retain_satisfied_with(&first_party, vec![&broken]).unwrap();
        assert!(kept.is_empty());
        assert_eq!(warnings[0].0, "acme/broken");
    }

    #[test]
    fn every_builtin_selection_endpoint_satisfies_its_own_prerequisites() {
        for selection in [
            hya_bundle::BuiltinBundleSelection::full(),
            hya_bundle::BuiltinBundleSelection::bare(),
        ] {
            let (kept, warnings) = retain_satisfied(&selection, Vec::new()).unwrap();
            assert!(kept.is_empty() && warnings.is_empty());
        }
    }
}
