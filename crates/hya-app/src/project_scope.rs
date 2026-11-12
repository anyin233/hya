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
        let inputs = ScopeInputs::scan(&base, roots.to_vec(), self.plugins.enabled()).await?;
        if !explicit
            && state.attempted == Some(inputs.fingerprint)
            && runtime.scope_overlay(&key).is_some()
        {
            return Ok(false);
        }
        state.attempted = Some(inputs.fingerprint);
        if !inputs.has_tiers() {
            let changed = runtime.scope_overlay(&key).is_some();
            runtime.drop_scope(&key);
            state.bundles.clear();
            state.plugins = ProjectPlugins::default();
            return Ok(changed);
        }
        let mut changed = runtime.scope_overlay(&key).is_none()
            || state.base_revision != base.revision
            || state.roots != roots;
        let retained_plugins = runtime
            .scope_overlay(&key)
            .map_or_else(Vec::new, |overlay| overlay.plugin_sources.clone());
        let (plugin_sources, mut errors, plugins_changed) = state
            .plugins
            .refresh(roots, &self.plugins, &retained_plugins, explicit)
            .await;
        errors.extend(base_error.map(|error| error.to_string()));
        changed |= plugins_changed;
        let dirs = project_bundle_dirs(roots, &state.bundles, &mut errors);
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
            let identity = hya_bundle::read_bundle_identity(dir)
                .map_err(|error| CoreError::Invalid(error.to_string()));
            let declared = identity
                .as_ref()
                .ok()
                .map(|(id, namespace)| (id.as_str(), namespace.as_str()));
            let retained = record.published.as_ref().map(|published| {
                let bundle = &published.catalog.bundles()[0];
                (bundle.identity().id.as_str(), bundle.namespace())
            });
            if let Some((id, namespace)) = declared.or(retained) {
                if ids.contains(id) || namespaces.contains(namespace) {
                    changed |= record.published.take().is_some();
                    record.attempted = None;
                    record.enabled = None;
                    record.error = None;
                    continue;
                }
                if !base.disabled.contains(id) {
                    ids.insert(id.to_string());
                    namespaces.insert(namespace.to_string());
                }
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
            let digest = bundle_input_digest(dir);
            let attempted = digest.as_ref().ok().copied();
            if enabled_changed
                || record.attempted != attempted
                || record.attempted.is_none()
                || (explicit && record.error.is_some())
            {
                record.attempted = attempted;
                let prepared = match digest {
                    Ok(_) => self.prepare_bundle(dir, &base.disabled).await,
                    Err(error) => Err(error),
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
            if let Some(error) = &record.error {
                errors.push(format!("project bundle {}: {error}", dir.display()));
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
                Err(CoreError::Invalid(errors.join("; ")))
            };
        }
        // Select catalogs using the same first-root/id/namespace precedence as
        // project loading, then installed bundles, then first-party bundles.
        let mut ids = BTreeSet::new();
        let mut namespaces = BTreeSet::new();
        let mut catalogs = Vec::new();
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
            if !published.enabled
                || base.disabled.contains(id)
                || ids.contains(id)
                || namespaces.contains(bundle.namespace())
            {
                continue;
            }
            ids.insert(id.clone());
            namespaces.insert(bundle.namespace().to_string());
            catalogs.push(&published.catalog);
            project_bundle_dirs.insert(id.clone(), dir.clone());
            if !published.models.is_empty() {
                bundle_models.insert(id.clone(), published.models.clone());
            }
            bundle_efforts.insert(id.clone(), published.efforts.clone());
            sources.extend(published.sources.clone());
        }
        for catalog in base.installed.iter().chain(&base.first_party) {
            let bundle = &catalog.bundles()[0];
            let id = &bundle.identity().id;
            if base.disabled.contains(id)
                || ids.contains(id)
                || namespaces.contains(bundle.namespace())
            {
                continue;
            }
            ids.insert(id.clone());
            namespaces.insert(bundle.namespace().to_string());
            catalogs.push(catalog.as_ref());
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
        let catalog = Arc::new(AgentCatalog::new(Arc::new(
            BundleCatalog::from_verified_catalogs(&catalogs)?,
        ))?);
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
            Err(CoreError::Invalid(errors.join("; ")))
        }
    }

    async fn prepare_bundle(
        &self,
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
        let id = catalog.bundles()[0].identity().id.clone();
        if disabled.contains(&id) {
            return Ok(PublishedBundle {
                enabled: false,
                catalog,
                sources: BTreeMap::new(),
                models: BTreeMap::new(),
                efforts: BTreeMap::new(),
            });
        }
        let agents = crate::agent_model_config::read_agent_settings(&dir.join("config.yml"))
            .map_err(|error| {
                CoreError::Invalid(format!("project bundle {id} configuration: {error:#}"))
            })?;
        let efforts = crate::agent_model_config::typed_efforts(&agents).map_err(|error| {
            CoreError::Invalid(format!("project bundle {id} configuration: {error:#}"))
        })?;
        let models = agents
            .into_iter()
            .filter_map(|(id, entry)| entry.model.map(|model| (id, ModelRef::new(model))))
            .collect();
        let resolver = BundleConfigResolver::new(
            self.installed.config_file().to_path_buf(),
            BTreeMap::from([(id, dir.to_path_buf())]),
        );
        let composition = compose(
            &[&catalog],
            &[],
            &[],
            &BTreeSet::new(),
            &resolver,
            self.installed.source_cache(),
            self.installed.host_reads(),
        )
        .await?;
        Ok(PublishedBundle {
            enabled: true,
            catalog,
            sources: composition.sources,
            models,
            efforts,
        })
    }
}

fn project_bundle_dirs(
    roots: &[PathBuf],
    retained: &BTreeMap<PathBuf, BundleRecord>,
    errors: &mut Vec<String>,
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
                errors.push(format!("read {}: {error}", directory.display()));
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

/// Change detector for one Project scope, computed at every bind without
/// preparing anything.
///
/// Extension point: every tier the overlay carries must fold its inputs into
/// `fingerprint` (and report itself through [`Self::has_tiers`]) so a change
/// republishes the Project. Project plugins add their `plugin.toml` digests
/// here.
struct ScopeInputs {
    /// Digest of the base revision, the root list, and every tier's inputs;
    /// published as [`ScopeOverlay::fingerprint`].
    fingerprint: [u8; 32],
    /// Whether any root has a project bundle directory.
    bundles: bool,
    /// Whether any root has a `plugin.toml` (and project plugins are on).
    plugins: bool,
}

impl ScopeInputs {
    async fn scan(
        base: &BaseCatalogs,
        roots: Vec<PathBuf>,
        plugins_enabled: bool,
    ) -> Result<Self, CoreError> {
        let revision = base.revision;
        tokio::task::spawn_blocking(move || {
            let mut plugin_hasher = sha2::Sha256::new();
            plugin_hasher.update(b"hya-project-plugins-v1\0");
            let mut plugins = false;
            if plugins_enabled {
                for id in crate::config::load_configured_plugin_ids() {
                    plugin_hasher.update(id.as_bytes());
                    plugin_hasher.update(b"\0");
                }
                for root in &roots {
                    plugin_hasher.update(b"root\0");
                    plugin_hasher.update(root.as_os_str().as_encoded_bytes());
                    plugins |= crate::plugins::project_plugins_digest(root, &mut plugin_hasher);
                }
            }
            let plugin_digest: [u8; 32] = plugin_hasher.finalize().into();
            let mut hasher = sha2::Sha256::new();
            hasher.update(b"hya-project-scope-v1\0");
            hasher.update(revision.to_le_bytes());
            let bundles = project_roots_digest(&roots, &mut hasher);
            hasher.update(plugin_digest);
            Ok(Self {
                fingerprint: hasher.finalize().into(),
                bundles,
                plugins,
            })
        })
        .await
        .map_err(|error| CoreError::Invalid(format!("scan Project roots: {error}")))?
    }

    /// Whether the Project has anything to overlay on the base.
    fn has_tiers(&self) -> bool {
        self.bundles || self.plugins
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
