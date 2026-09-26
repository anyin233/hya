//! Catalog scopes: which catalog tier a turn binds.
//!
//! The [`crate::RuntimeRegistry`] keeps one process-wide base snapshot. A
//! [`CatalogScope`] names the view a turn wants on top of it: the base alone
//! ([`CatalogScope::Global`] and [`CatalogScope::Directory`] without an
//! overlay) or the base plus one published [`ScopeOverlay`] (typically a
//! registered Project's bundles and plugins). Overlays are published and
//! dropped by [`ScopeKey`]; the registry composes each scope snapshot lazily
//! and rebuilds it when the base generation moves.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hya_proto::{ModelRef, ProjectId};

use crate::agent_catalog::AgentCatalog;
use crate::runtime_registry::RuntimeSource;

/// The catalog tier one turn or catalog read binds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CatalogScope {
    /// No directory: the base snapshot (user skills only).
    Global,
    /// A directory that belongs to no registered Project: the base snapshot
    /// with that directory's inert tiers (skills keyed by the workdir).
    Directory(PathBuf),
    /// A registered Project: the base snapshot plus the Project's overlay.
    Project {
        /// Project identity; the overlay key.
        id: ProjectId,
        /// The Project's workspace roots in order (first root wins).
        roots: Vec<PathBuf>,
    },
}

impl CatalogScope {
    /// The overlay key for this scope. Project roots are not part of the
    /// key: one Project has one overlay whatever its current root list.
    #[must_use]
    pub fn key(&self) -> ScopeKey {
        match self {
            Self::Global => ScopeKey::Global,
            Self::Directory(path) => ScopeKey::Directory(path.clone()),
            Self::Project { id, .. } => ScopeKey::Project(*id),
        }
    }

    /// The Project id, when this is a Project scope.
    #[must_use]
    pub fn project_id(&self) -> Option<ProjectId> {
        match self {
            Self::Project { id, .. } => Some(*id),
            Self::Global | Self::Directory(_) => None,
        }
    }

    /// The scope's roots: a Project's roots, a Directory's path, or none.
    #[must_use]
    pub fn roots(&self) -> &[PathBuf] {
        match self {
            Self::Global => &[],
            Self::Directory(path) => std::slice::from_ref(path),
            Self::Project { roots, .. } => roots,
        }
    }
}

/// Identity of one scope overlay in the registry.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ScopeKey {
    /// The global scope.
    Global,
    /// One directory outside every registered Project.
    Directory(PathBuf),
    /// One registered Project.
    Project(ProjectId),
}

impl std::fmt::Display for ScopeKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Global => formatter.write_str("global"),
            Self::Directory(path) => write!(formatter, "directory:{}", path.display()),
            Self::Project(id) => write!(formatter, "project:{id}"),
        }
    }
}

/// Everything one scope adds on top of the base snapshot.
///
/// The scope snapshot is the base snapshot with `catalog` replacing the
/// catalog, `bundle_sources` replacing every Bundle-kind source, and
/// `plugin_sources` added (a scope Plugin source whose id the base already
/// publishes is skipped: configured plugins beat project manifests). The
/// same validation as a base publication applies.
#[derive(Clone)]
pub struct ScopeOverlay {
    /// The complete Agent/Bundle catalog of this scope (installed plus
    /// scope bundles).
    pub catalog: Arc<AgentCatalog>,
    /// Every Bundle-kind source of this scope (installed plus scope
    /// bundles); replaces the base's Bundle sources.
    pub bundle_sources: Vec<RuntimeSource>,
    /// Extra Plugin-kind sources (typically project plugins carrying hooks).
    /// Their hooks reach only bindings of this scope.
    pub plugin_sources: Vec<RuntimeSource>,
    /// Model leaves of the scope's bundles (`bundle id -> agent id ->
    /// model`), read from each scope bundle's own `config.yml`.
    pub bundle_models: BTreeMap<String, BTreeMap<String, ModelRef>>,
    /// Scope bundle id to the bundle's source directory. These ids shadow
    /// the user-scope model configuration of the same bundle id.
    pub project_bundle_dirs: BTreeMap<String, PathBuf>,
    /// Caller-defined digest of the inputs this overlay was built from;
    /// compare it with [`crate::RuntimeRegistry::scope_overlay`] to skip a
    /// rebuild. The registry never interprets it.
    pub fingerprint: [u8; 32],
}

impl ScopeOverlay {
    /// An overlay with `catalog` and nothing else.
    #[must_use]
    pub fn new(catalog: Arc<AgentCatalog>) -> Self {
        Self {
            catalog,
            bundle_sources: Vec::new(),
            plugin_sources: Vec::new(),
            bundle_models: BTreeMap::new(),
            project_bundle_dirs: BTreeMap::new(),
            fingerprint: [0; 32],
        }
    }

    /// Whether `bundle_id` is a scope bundle (it has a source directory).
    #[must_use]
    pub fn is_project_bundle(&self, bundle_id: &str) -> bool {
        self.project_bundle_dirs.contains_key(bundle_id)
    }

    /// The source directory of scope bundle `bundle_id`.
    #[must_use]
    pub fn project_bundle_dir(&self, bundle_id: &str) -> Option<&Path> {
        self.project_bundle_dirs
            .get(bundle_id)
            .map(PathBuf::as_path)
    }
}
