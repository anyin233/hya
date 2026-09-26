//! Per-Project catalog tier: the project bundles of a registered Project.
//!
//! [`ProjectScopeRefresh`] wraps the process-wide [`InstalledBundleRefresh`]
//! (installed plus first-party bundles, the base every scope shares) and, for
//! a [`CatalogScope::Project`], publishes a [`ScopeOverlay`] holding:
//!
//! * the complete scope catalog: the Project's bundles from
//!   `<root>/.hya/bundles` of every root in order (first root wins on bundle
//!   id or namespace) shadowing installed and first-party bundles by id or
//!   namespace;
//! * every Bundle-kind runtime source of the scope. Sources come from one
//!   cache keyed by (bundle id, runtime fingerprint), so an installed bundle's
//!   process is shared by the base and every scope while a project bundle's
//!   process (its configuration lives in its own source directory) belongs to
//!   the Project that loads it and stops once that scope is dropped and no
//!   binding retains it;
//! * the project bundles' model leaves and source directories, read from
//!   each project bundle's own `config.yml`.
//!
//! Directory, Global, and temporary scopes load no project tier: executable
//! project code only runs for registered Projects.
//!
//! Each bind scans the Project roots without preparing anything and compares
//! the digest with the published overlay's fingerprint; only a change (or an
//! evicted overlay, or a new base publication) rebuilds and republishes that
//! one Project. A per-scope lock keeps concurrent binds of one Project from
//! starting its processes twice.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::PreparedCatalog;
use hya_core::{
    AgentCatalog, CatalogScope, CoreError, RuntimeCatalogRefresh, RuntimeRegistry, ScopeKey,
    ScopeOverlay,
};
use hya_proto::ModelRef;
use sha2::Digest as _;

use crate::agent_model_config::AgentModelConfigFiles;
use crate::bundle_config::BundleConfigResolver;
use crate::installed_bundle_refresh::{BaseCatalogs, InstalledBundleRefresh, compose};
use crate::project_bundles::{load_project_bundle_dirs_for_roots, project_roots_digest};

/// Refreshes the base catalog and the per-Project bundle tier.
///
/// Register this (not the wrapped [`InstalledBundleRefresh`]) as the engine's
/// catalog refresh: `refresh_if_changed` refreshes the base,
/// `refresh_scope` the Project overlay.
pub struct ProjectScopeRefresh {
    installed: Arc<InstalledBundleRefresh>,
    locks: std::sync::Mutex<HashMap<ScopeKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl ProjectScopeRefresh {
    /// Compose Project overlays on top of `installed`'s base catalog.
    #[must_use]
    pub fn new(installed: Arc<InstalledBundleRefresh>) -> Self {
        Self {
            installed,
            locks: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The wrapped base refresh.
    #[must_use]
    pub fn installed(&self) -> &Arc<InstalledBundleRefresh> {
        &self.installed
    }

    fn scope_lock(&self, key: &ScopeKey) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            self.locks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(key.clone())
                .or_default(),
        )
    }

    /// Drop cached sources and idle locks of scopes the registry no longer
    /// holds (dropped or evicted overlays).
    fn collect(&self, runtime: &RuntimeRegistry) {
        let live = runtime.scope_keys();
        self.installed.source_cache().collect(&live);
        self.locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|key, lock| live.contains(key) || Arc::strong_count(lock) > 1);
    }

    /// Bring one Project's overlay up to date.
    async fn refresh_project(
        &self,
        runtime: &RuntimeRegistry,
        key: ScopeKey,
        roots: &[PathBuf],
    ) -> Result<bool, CoreError> {
        let lock = self.scope_lock(&key);
        let _serialized = lock.lock().await;
        if self.installed.base_catalogs().revision == 0 {
            self.installed.refresh_if_changed(runtime).await?;
        }
        let base = self.installed.base_catalogs();
        let inputs = ScopeInputs::scan(&base, roots.to_vec()).await?;
        let current = runtime.scope_overlay(&key);
        if !inputs.has_tiers() {
            // Nothing project-specific: the scope binds the base snapshot.
            if current.is_none() {
                return Ok(false);
            }
            runtime.drop_scope(&key);
            self.installed.source_cache().collect(&runtime.scope_keys());
            return Ok(true);
        }
        if current.is_some_and(|overlay| overlay.fingerprint == inputs.fingerprint) {
            return Ok(false);
        }

        let tier = ProjectTier::load(roots.to_vec()).await?;
        let resolver = BundleConfigResolver::new(
            self.installed.config_file().to_path_buf(),
            tier.project_bundle_dirs.clone(),
        );
        let project = tier
            .catalogs
            .iter()
            .map(|(catalog, _)| catalog)
            .collect::<Vec<_>>();
        let installed = base.installed.iter().map(Arc::as_ref).collect::<Vec<_>>();
        let first_party = base.first_party.iter().map(Arc::as_ref).collect::<Vec<_>>();
        let composition = compose(
            &project,
            &installed,
            &first_party,
            &resolver,
            self.installed.source_cache(),
            self.installed.host_reads(),
        )
        .await?;
        let catalog = Arc::new(AgentCatalog::new(Arc::clone(&composition.bundles))?);
        let overlay = ScopeOverlay {
            catalog,
            bundle_sources: composition.runtime_sources(),
            // S4: the Project's plugin sources (with hooks) go here.
            plugin_sources: Vec::new(),
            bundle_models: tier.bundle_models,
            project_bundle_dirs: tier.project_bundle_dirs,
            fingerprint: inputs.fingerprint,
        };
        runtime.publish_scope(key.clone(), overlay)?;
        self.installed
            .source_cache()
            .commit_scope(key, composition.sources, &runtime.scope_keys());
        Ok(true)
    }
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
}

impl ScopeInputs {
    async fn scan(base: &BaseCatalogs, roots: Vec<PathBuf>) -> Result<Self, CoreError> {
        let revision = base.revision;
        tokio::task::spawn_blocking(move || {
            let mut hasher = sha2::Sha256::new();
            hasher.update(b"hya-project-scope-v1\0");
            hasher.update(revision.to_le_bytes());
            let bundles = project_roots_digest(&roots, &mut hasher);
            Self {
                fingerprint: hasher.finalize().into(),
                bundles,
            }
        })
        .await
        .map_err(|error| CoreError::Invalid(format!("scan Project roots: {error}")))
    }

    /// Whether the Project has anything to overlay on the base.
    fn has_tiers(&self) -> bool {
        self.bundles
    }
}

/// The loaded project bundle tier of one Project.
struct ProjectTier {
    /// Kept project bundles (first root wins) with their source directories.
    catalogs: Vec<(PreparedCatalog, PathBuf)>,
    /// Project bundle id to source directory.
    project_bundle_dirs: BTreeMap<String, PathBuf>,
    /// Model leaves from each project bundle's `config.yml`.
    bundle_models: BTreeMap<String, BTreeMap<String, ModelRef>>,
}

impl ProjectTier {
    async fn load(roots: Vec<PathBuf>) -> Result<Self, CoreError> {
        tokio::task::spawn_blocking(move || {
            let (catalogs, _) = load_project_bundle_dirs_for_roots(&roots);
            let project_bundle_dirs = catalogs
                .iter()
                .filter_map(|(catalog, dir)| {
                    let bundle = catalog.bundles().first()?;
                    Some((bundle.identity().id.clone(), dir.clone()))
                })
                .collect::<BTreeMap<_, _>>();
            let mut bundle_models = BTreeMap::new();
            for (bundle_id, dir) in &project_bundle_dirs {
                match AgentModelConfigFiles::project_models(dir) {
                    Ok(models) if !models.is_empty() => {
                        bundle_models.insert(bundle_id.clone(), models);
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(
                        bundle_id = %bundle_id,
                        dir = %dir.display(),
                        error = %format!("{error:#}"),
                        "ignoring the project bundle's malformed model configuration"
                    ),
                }
            }
            Self {
                catalogs,
                project_bundle_dirs,
                bundle_models,
            }
        })
        .await
        .map_err(|error| CoreError::Invalid(format!("load Project bundles: {error}")))
    }
}

#[async_trait]
impl RuntimeCatalogRefresh for ProjectScopeRefresh {
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        self.installed.refresh_if_changed(runtime).await
    }

    async fn refresh_scope(
        &self,
        runtime: &RuntimeRegistry,
        scope: &CatalogScope,
    ) -> Result<bool, CoreError> {
        self.collect(runtime);
        match scope {
            CatalogScope::Project { roots, .. } => {
                self.refresh_project(runtime, scope.key(), roots).await
            }
            // No code tiers outside registered Projects.
            CatalogScope::Global | CatalogScope::Directory(_) => Ok(false),
        }
    }
}
