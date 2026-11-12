//! Per-Project extension refresh. Each declaration remembers its last attempt
//! and last published source. Failed edits retain that source independently;
//! only changed inputs or an explicit refresh retry preparation.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{BundleCatalog, PreparedCatalog};
use hya_core::{
    AgentCatalog, CatalogScope, CoreError, RuntimeCatalogRefresh, RuntimeRegistry, ScopeKey,
    ScopeOverlay,
};
use hya_proto::ModelRef;
use sha2::Digest as _;

use crate::bundle_config::BundleConfigResolver;
use crate::installed_bundle_refresh::{BaseCatalogs, InstalledBundleRefresh, SourceKey, compose};
use crate::project_bundles::{
    directory_digest, directory_source, project_roots_digest, root_bundles_dir,
};
use crate::project_plugins::{ProjectPluginSettings, ProjectPlugins};

/// Refreshes installed bundles and independently retained Project sources.
pub struct ProjectScopeRefresh {
    installed: Arc<InstalledBundleRefresh>,
    plugins: ProjectPluginSettings,
    scopes: std::sync::Mutex<HashMap<ScopeKey, Arc<tokio::sync::Mutex<ProjectSources>>>>,
}

#[derive(Default)]
struct ProjectSources {
    base_revision: u64,
    roots: Vec<PathBuf>,
    attempted: Option<[u8; 32]>,
    bundle_inputs: Option<[u8; 32]>,
    plugins: ProjectPlugins,
    bundles: BTreeMap<PathBuf, BundleRecord>,
}

struct BundleRecord {
    enabled: Option<bool>,
    attempted: Option<[u8; 32]>,
    published: Option<PublishedBundle>,
    error: Option<String>,
}

struct PublishedBundle {
    enabled: bool,
    catalog: PreparedCatalog,
    sources: BTreeMap<SourceKey, hya_core::RuntimeSource>,
    models: BTreeMap<String, ModelRef>,
    efforts: BTreeMap<String, hya_provider::ReasoningEffort>,
}

impl ProjectScopeRefresh {
    /// Prepare a fresh scope's inert bundle catalog without starting runtime
    /// sources. Roots are ordered; failed sources are isolated and returned as
    /// warnings. Pass no roots for a Directory or Global scope.
    ///
    /// # Errors
    /// Returns catalog validation errors after selecting readable sources.
    pub fn catalog_for_roots(
        roots: &[PathBuf],
        installed: &[PreparedCatalog],
        first_party: &[PreparedCatalog],
        disabled: &BTreeSet<String>,
    ) -> Result<(BundleCatalog, Vec<String>), CoreError> {
        let mut errors = Vec::new();
        let dirs = project_bundle_dirs(roots, &BTreeMap::new(), &mut errors);
        let mut ids = BTreeSet::new();
        let mut namespaces = BTreeSet::new();
        let mut project = Vec::new();
        for dir in dirs {
            if let Ok((id, namespace)) = hya_bundle::read_bundle_identity(&dir)
                && !claim_bundle(&id, &namespace, disabled, &mut ids, &mut namespaces)
            {
                continue;
            }
            match prepare_bundle_catalog(&dir, disabled) {
                Ok(bundle) => project.push(bundle.catalog),
                Err(error) => errors.push((
                    dir.display().to_string(),
                    format!("project bundle {}: {error}", dir.display()),
                )),
            }
        }
        let (catalog, _) = compose_scope_catalog(
            project.iter(),
            installed.iter().chain(first_party),
            disabled,
        )?;
        let warnings = errors.into_iter().map(|(_, message)| message).collect();
        Ok((catalog, warnings))
    }

    /// Compose Project sources on top of the installed base catalog.
    #[must_use]
    pub fn new(installed: Arc<InstalledBundleRefresh>) -> Self {
        Self {
            installed,
            plugins: ProjectPluginSettings::default(),
            scopes: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Configure which project plugin declarations can load.
    #[must_use]
    pub fn with_project_plugins(mut self, settings: ProjectPluginSettings) -> Self {
        self.plugins = settings;
        self
    }

    /// The process-wide installed-bundle refresh.
    #[must_use]
    pub fn installed(&self) -> &Arc<InstalledBundleRefresh> {
        &self.installed
    }

    fn scope_state(&self, key: &ScopeKey) -> Arc<tokio::sync::Mutex<ProjectSources>> {
        self.scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key.clone())
            .or_default()
            .clone()
    }

    fn collect(&self, runtime: &RuntimeRegistry) {
        let live = runtime.scope_keys();
        self.installed.source_cache().collect(&live);
        self.scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|key, state| live.contains(key) || Arc::strong_count(state) > 1);
    }

    async fn refresh_project(
        &self,
        runtime: &RuntimeRegistry,
        key: ScopeKey,
        roots: &[PathBuf],
        explicit: bool,
    ) -> Result<bool, CoreError> {
        let state = self.scope_state(&key);
        let mut state = state.lock().await;
        let base_error = if self.installed.base_catalogs().revision == 0 {
            self.installed.refresh_if_changed(runtime).await.err()
        } else {
            None
        };
        let base = self.installed.base_catalogs();
        let base_error = match (base.revision, base_error) {
            (0, Some(error)) => return Err(error),
            (_, error) => error,
        };
        let scan_bundles = explicit || state.bundle_inputs.is_none() || state.roots != roots;
        let cached_bundles = if scan_bundles {
            None
        } else {
            state.bundle_inputs
        };
        let inputs = ScopeInputs::scan(
            &base,
            roots.to_vec(),
            self.plugins.enabled(),
            cached_bundles,
        )
        .await?;
        state.bundle_inputs = Some(inputs.bundle_inputs);
        if !explicit
            && state.attempted == Some(inputs.fingerprint)
            && runtime.scope_overlay(&key).is_some()
        {
            return Ok(false);
        }
        state.attempted = Some(inputs.fingerprint);
        let mut changed = runtime.scope_overlay(&key).is_none()
            || state.base_revision != base.revision
            || state.roots != roots;
        let retained_plugins = runtime
            .scope_overlay(&key)
            .map_or_else(Vec::new, |overlay| overlay.plugin_sources.clone());
        let (plugin_sources, plugin_errors, plugins_changed) = state
            .plugins
            .refresh(roots, &self.plugins, &retained_plugins, explicit)
            .await;
        let mut errors = plugin_errors
            .into_iter()
            .map(|error| (key.to_string(), error))
            .collect::<Vec<_>>();
        if let Some(error) = base_error {
            match error {
                CoreError::BundleRefresh(failures) => errors.extend(failures),
                CoreError::BundleRuntime { bundle_id, source } => {
                    errors.push((bundle_id, source.to_string()))
                }
                other => errors.push((key.to_string(), other.to_string())),
            }
        }
        changed |= plugins_changed;
        let dirs = if scan_bundles {
            project_bundle_dirs(roots, &state.bundles, &mut errors)
        } else {
            roots
                .iter()
                .flat_map(|root| {
                    let directory = root_bundles_dir(root);
                    state
                        .bundles
                        .keys()
                        .filter(move |dir| dir.starts_with(&directory))
                        .cloned()
                })
                .collect()
        };
        let present = dirs.iter().cloned().collect::<BTreeSet<_>>();
        state.bundles.retain(|dir, record| {
            let keep = present.contains(dir);
            changed |= !keep && record.published.is_some();
            keep
        });
        let mut ids = BTreeSet::new();
        let mut namespaces = BTreeSet::new();
        for dir in &dirs {
            let record = state.bundles.entry(dir.clone()).or_insert(BundleRecord {
                enabled: None,
                attempted: None,
                published: None,
                error: None,
            });
            let identity = if scan_bundles {
                hya_bundle::read_bundle_identity(dir).ok()
            } else {
                record.published.as_ref().map(|published| {
                    let bundle = &published.catalog.bundles()[0];
                    (bundle.identity().id.clone(), bundle.namespace().to_string())
                })
            };
            let declared = identity
                .as_ref()
                .map(|(id, namespace)| (id.as_str(), namespace.as_str()));
            let retained = record.published.as_ref().map(|published| {
                let bundle = &published.catalog.bundles()[0];
                (bundle.identity().id.as_str(), bundle.namespace())
            });
            let source_id = declared
                .or(retained)
                .map_or_else(|| dir.display().to_string(), |(id, _)| id.to_string());
            if let Some((id, namespace)) = declared.or(retained)
                && !claim_bundle(id, namespace, &base.disabled, &mut ids, &mut namespaces)
            {
                changed |= record.published.take().is_some();
                record.attempted = None;
                record.enabled = None;
                record.error = None;
                continue;
            }
            let enabled = record.published.as_ref().map(|published| {
                !base
                    .disabled
                    .contains(&published.catalog.bundles()[0].identity().id)
            });
            let enabled_changed = record.enabled != enabled;
            record.enabled = enabled;
            if enabled == Some(false)
                && let Some(published) = &mut record.published
                && published.enabled
            {
                published.enabled = false;
                published.sources.clear();
                changed = true;
            }
            let digest = scan_bundles.then(|| bundle_input_digest(dir));
            let attempted = match &digest {
                Some(digest) => digest.as_ref().ok().copied(),
                None => record.attempted,
            };
            if enabled_changed
                || (scan_bundles && (record.attempted != attempted || record.attempted.is_none()))
                || (explicit && record.error.is_some())
            {
                record.attempted = attempted;
                let prepared = match digest {
                    Some(Err(error)) => Err(error),
                    _ => self.prepare_bundle(dir, &base.disabled).await,
                };
                match prepared {
                    Ok(bundle) => {
                        record.enabled = Some(bundle.enabled);
                        changed = true;
                        record.published = Some(bundle);
                        record.error = None;
                    }
                    Err(error) => {
                        record.error = Some(error.to_string());
                    }
                }
            }
            if let Some(error) = &record.error
                && !base.disabled.contains(&source_id)
            {
                errors.push((
                    source_id,
                    format!("project bundle {}: {error}", dir.display()),
                ));
            }
            if let Some(published) = &record.published
                && published.enabled
            {
                let bundle = &published.catalog.bundles()[0];
                ids.insert(bundle.identity().id.clone());
                namespaces.insert(bundle.namespace().to_string());
            }
        }
        if !changed {
            return if errors.is_empty() {
                Ok(false)
            } else {
                Err(CoreError::BundleRefresh(errors))
            };
        }
        // Select catalogs using the same first-root/id/namespace precedence as
        // project loading, then installed bundles, then first-party bundles.
        let (bundles, project_catalogs) = compose_scope_catalog(
            dirs.iter().filter_map(|dir| {
                state.bundles[dir]
                    .published
                    .as_ref()
                    .filter(|bundle| bundle.enabled)
                    .map(|bundle| &bundle.catalog)
            }),
            base.installed
                .iter()
                .chain(&base.first_party)
                .map(Arc::as_ref),
            &base.disabled,
        )?;
        let ids = bundles
            .bundles()
            .iter()
            .map(|bundle| bundle.identity().id.clone())
            .collect::<BTreeSet<_>>();
        let mut bundle_models = BTreeMap::new();
        let mut bundle_efforts = BTreeMap::new();
        let mut project_bundle_dirs = BTreeMap::new();
        let mut sources = BTreeMap::new();
        for dir in &dirs {
            let Some(published) = state.bundles[dir].published.as_ref() else {
                continue;
            };
            let bundle = &published.catalog.bundles()[0];
            let id = &bundle.identity().id;
            // Failed edits can retain colliding old identities. Match the
            // selected source itself when carrying its runtime metadata.
            if !project_catalogs
                .iter()
                .any(|catalog| std::ptr::eq(*catalog, &published.catalog))
            {
                continue;
            }
            project_bundle_dirs.insert(id.clone(), dir.clone());
            if !published.models.is_empty() {
                bundle_models.insert(id.clone(), published.models.clone());
            }
            bundle_efforts.insert(id.clone(), published.efforts.clone());
            sources.extend(published.sources.clone());
        }
        let mut bundle_sources = sources.values().cloned().collect::<Vec<_>>();
        bundle_sources.extend(
            runtime
                .published_bundle_sources()
                .into_iter()
                .filter(|source| {
                    ids.contains(source.id().configured_id())
                        && !project_bundle_dirs.contains_key(source.id().configured_id())
                }),
        );
        let catalog = Arc::new(AgentCatalog::new(Arc::new(bundles))?);
        runtime.publish_scope(
            key.clone(),
            ScopeOverlay {
                catalog,
                bundle_sources,
                plugin_sources,
                bundle_models,
                bundle_efforts,
                project_bundle_dirs,
                fingerprint: inputs.fingerprint,
            },
        )?;
        self.installed
            .source_cache()
            .commit_scope(key, sources, &runtime.scope_keys());
        state.base_revision = base.revision;
        state.roots = roots.to_vec();
        if errors.is_empty() {
            Ok(true)
        } else {
            Err(CoreError::BundleRefresh(errors))
        }
    }

    async fn prepare_bundle(
        &self,
        dir: &std::path::Path,
        disabled: &BTreeSet<String>,
    ) -> Result<PublishedBundle, CoreError> {
        let mut published = prepare_bundle_catalog(dir, disabled)?;
        if !published.enabled {
            return Ok(published);
        }
        let id = published.catalog.bundles()[0].identity().id.clone();
        let resolver = BundleConfigResolver::new(
            self.installed.config_file().to_path_buf(),
            BTreeMap::from([(id, dir.to_path_buf())]),
        );
        let composition = compose(
            &[&published.catalog],
            &[],
            &[],
            &BTreeSet::new(),
            &resolver,
            self.installed.source_cache(),
            self.installed.host_reads(),
        )
        .await?;
        published.sources = composition.sources;
        Ok(published)
    }
}

fn prepare_bundle_catalog(
    dir: &std::path::Path,
    disabled: &BTreeSet<String>,
) -> Result<PublishedBundle, CoreError> {
    let source = directory_source(dir)
        .map_err(|error| CoreError::Invalid(error.to_string()))?
        .ok_or_else(|| {
            CoreError::Invalid(format!(
                "project bundle {} has no bundle.yaml or bundle.hya.md",
                dir.display()
            ))
        })?;
    let catalog = hya_bundle::prepare_package(source)
        .map_err(|error| CoreError::Invalid(error.to_string()))?;
    if catalog.bundles().len() != 1 {
        return Err(CoreError::Invalid(
            "project bundle must prepare exactly one bundle".into(),
        ));
    }
    let id = &catalog.bundles()[0].identity().id;
    let enabled = !disabled.contains(id);
    let agents = if enabled {
        crate::agent_model_config::read_agent_settings(&dir.join("config.yml")).map_err(
            |error| CoreError::Invalid(format!("project bundle {id} configuration: {error:#}")),
        )?
    } else {
        BTreeMap::new()
    };
    let efforts = crate::agent_model_config::typed_efforts(&agents).map_err(|error| {
        CoreError::Invalid(format!("project bundle {id} configuration: {error:#}"))
    })?;
    let models = agents
        .into_iter()
        .filter_map(|(id, entry)| entry.model.map(|model| (id, ModelRef::new(model))))
        .collect();
    Ok(PublishedBundle {
        enabled,
        catalog,
        sources: BTreeMap::new(),
        models,
        efforts,
    })
}

fn claim_bundle(
    id: &str,
    namespace: &str,
    disabled: &BTreeSet<String>,
    ids: &mut BTreeSet<String>,
    namespaces: &mut BTreeSet<String>,
) -> bool {
    if ids.contains(id) || namespaces.contains(namespace) {
        return false;
    }
    if !disabled.contains(id) {
        ids.insert(id.to_string());
        namespaces.insert(namespace.to_string());
    }
    true
}

fn compose_scope_catalog<'a>(
    project: impl Iterator<Item = &'a PreparedCatalog>,
    base: impl Iterator<Item = &'a PreparedCatalog>,
    disabled: &BTreeSet<String>,
) -> Result<(BundleCatalog, Vec<&'a PreparedCatalog>), CoreError> {
    let mut ids = BTreeSet::new();
    let mut namespaces = BTreeSet::new();
    let mut select = |catalog: &&PreparedCatalog| {
        let bundle = &catalog.bundles()[0];
        let id = &bundle.identity().id;
        !disabled.contains(id)
            && claim_bundle(id, bundle.namespace(), disabled, &mut ids, &mut namespaces)
    };
    let mut catalogs = project.filter(&mut select).collect::<Vec<_>>();
    let project_catalogs = catalogs.clone();
    catalogs.extend(base.filter(select));
    Ok((
        BundleCatalog::from_verified_catalogs(&catalogs)?,
        project_catalogs,
    ))
}

fn project_bundle_dirs(
    roots: &[PathBuf],
    retained: &BTreeMap<PathBuf, BundleRecord>,
    errors: &mut Vec<(String, String)>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for root in roots {
        let directory = root_bundles_dir(root);
        let paths = match std::fs::read_dir(&directory) {
            Ok(entries) => entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => Err(error),
        };
        match paths {
            Ok(mut paths) => {
                paths.retain(|path| path.is_dir());
                paths.sort();
                dirs.extend(paths);
            }
            Err(error) => {
                errors.push((
                    directory.display().to_string(),
                    format!("read {}: {error}", directory.display()),
                ));
                dirs.extend(
                    retained
                        .keys()
                        .filter(|dir| dir.starts_with(&directory))
                        .cloned(),
                );
            }
        }
    }
    dirs
}

fn bundle_input_digest(dir: &std::path::Path) -> Result<[u8; 32], CoreError> {
    let mut digest = sha2::Sha256::new();
    digest.update(
        directory_digest(dir)
            .map_err(|error| CoreError::Invalid(error.to_string()))?
            .to_le_bytes(),
    );
    match std::fs::read(dir.join(crate::bundle_config::BUNDLE_CONFIG_FILE_NAME)) {
        Ok(bytes) => digest.update(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(CoreError::Invalid(error.to_string())),
    }
    Ok(digest.finalize().into())
}

/// Binding inputs: live plugin declarations and the last explicitly scanned
/// bundle sources. Bundle bytes are read only on first use, root changes or
/// explicit refresh.
struct ScopeInputs {
    /// Digest of the base revision, the root list, and every tier's inputs;
    /// published as [`ScopeOverlay::fingerprint`].
    fingerprint: [u8; 32],
    bundle_inputs: [u8; 32],
}

impl ScopeInputs {
    async fn scan(
        base: &BaseCatalogs,
        roots: Vec<PathBuf>,
        plugins_enabled: bool,
        cached_bundles: Option<[u8; 32]>,
    ) -> Result<Self, CoreError> {
        let revision = base.revision;
        tokio::task::spawn_blocking(move || {
            let mut plugin_hasher = sha2::Sha256::new();
            plugin_hasher.update(b"hya-project-plugins-v1\0");
            if plugins_enabled {
                for id in crate::config::load_configured_plugin_ids() {
                    plugin_hasher.update(id.as_bytes());
                    plugin_hasher.update(b"\0");
                }
                for root in &roots {
                    plugin_hasher.update(b"root\0");
                    plugin_hasher.update(root.as_os_str().as_encoded_bytes());
                    crate::plugins::project_plugins_digest(root, &mut plugin_hasher);
                }
            }
            let plugin_digest: [u8; 32] = plugin_hasher.finalize().into();
            let mut hasher = sha2::Sha256::new();
            hasher.update(b"hya-project-scope-v1\0");
            hasher.update(revision.to_le_bytes());
            let bundle_inputs = cached_bundles.unwrap_or_else(|| {
                let mut digest = sha2::Sha256::new();
                project_roots_digest(&roots, &mut digest);
                <[u8; 32]>::from(digest.finalize())
            });
            hasher.update(bundle_inputs);
            hasher.update(plugin_digest);
            Ok(Self {
                fingerprint: hasher.finalize().into(),
                bundle_inputs,
            })
        })
        .await
        .map_err(|error| CoreError::Invalid(format!("scan Project roots: {error}")))?
    }
}

#[async_trait]
impl RuntimeCatalogRefresh for ProjectScopeRefresh {
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        self.installed.refresh_if_changed(runtime).await
    }

    async fn refresh_explicit(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        self.installed.refresh_explicit(runtime).await
    }

    async fn refresh_scope(
        &self,
        runtime: &RuntimeRegistry,
        scope: &CatalogScope,
    ) -> Result<bool, CoreError> {
        self.collect(runtime);
        match scope {
            CatalogScope::Project { roots, .. } => {
                self.refresh_project(runtime, scope.key(), roots, false)
                    .await
            }
            CatalogScope::Global | CatalogScope::Directory(_) => Ok(false),
        }
    }

    async fn refresh_scope_explicit(
        &self,
        runtime: &RuntimeRegistry,
        scope: &CatalogScope,
    ) -> Result<bool, CoreError> {
        self.collect(runtime);
        match scope {
            CatalogScope::Project { roots, .. } => {
                self.refresh_project(runtime, scope.key(), roots, true)
                    .await
            }
            CatalogScope::Global | CatalogScope::Directory(_) => Ok(false),
        }
    }
}
