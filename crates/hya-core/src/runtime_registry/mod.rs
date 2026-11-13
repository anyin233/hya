use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, Weak};

use hya_bundle::{BundleCatalog, BundleError, ExportKind, ResourceView};

use crate::agent_catalog::{AgentCatalog, AgentDefinition, AgentOrigin};
use crate::catalog_scope::{CatalogScope, ScopeKey, ScopeOverlay};
use hya_proto::{ConfigGeneration, ModelRef, ToolName, ToolSchema};
use hya_provider::ReasoningEffort;
use hya_tool::{
    DuplicateName, NamedTool, PermissionPlane, ResolvedTool, SkillCatalogEntry, SkillPlane, Tool,
    ToolPermission, ToolRegistry, ToolRegistrySnapshot, discover_skills_with_builtins,
    handle::{
        SchemeBinding, SchemeDispatch, SchemeHandler, SchemeReadTool, SchemeRegistry,
        SchemeWriteTool,
    },
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::watch;

mod compile;
mod prompt;
mod sources;

use compile::BindingPlace;
pub use compile::{AgentResourcePolicy, AgentToolPlane, TurnBinding};
pub(crate) use compile::{BoundAgentHook, CompiledResourceView};
use sources::*;
pub use sources::{
    RuntimeCandidate, RuntimeSource, RuntimeSourceExport, RuntimeSourceId, RuntimeSourceKind,
    RuntimeSourceManifest, RuntimeSourceOwner, RuntimeSourceSkill, SourceSchema,
};

/// Complete user-file model configuration captured by a runtime binding.
///
/// Built-in Agents share the global Hya configuration file, while bundle
/// Agents are partitioned by their owning stable bundle identity. `BTreeMap`
/// keeps the representation and every derived identity deterministic.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AgentModelConfiguration {
    /// Stable built-in Agent id to configured model.
    pub builtin: BTreeMap<String, ModelRef>,
    /// Stable bundle id to Agent id to configured model.
    pub bundles: BTreeMap<String, BTreeMap<String, ModelRef>>,
}

type AgentModelPreferenceSnapshot = Arc<BTreeMap<String, ModelRef>>;
type AgentModelConfigurationSnapshot = Arc<AgentModelConfiguration>;
type AgentModelPreferences = watch::Sender<AgentModelPreferenceSnapshot>;
type AgentModelConfigurations = watch::Sender<AgentModelConfigurationSnapshot>;

/// The published bare-name mask table: contested bare name → the canonical
/// name of the winning tool. Derived from the published source claims at
/// publication time and immutable per snapshot.
type SnapshotMasks = Arc<BTreeMap<String, String>>;

/// The published scheme table: registered external URI scheme → the winning
/// source's binding. Mirrors [`SnapshotMasks`]: derived from the published
/// source claims at publication time and immutable per snapshot.
type SnapshotSchemes = Arc<BTreeMap<String, SchemeBinding>>;

/// A complete immutable configuration view. Turns retain its `Arc` for their
/// whole lifetime, so publication cannot alter an in-flight lookup.
struct RuntimeSnapshot {
    generation: ConfigGeneration,
    catalog: Arc<AgentCatalog>,
    basic_tools: ToolRegistrySnapshot,
    tools: ToolRegistrySnapshot,
    skills: BTreeMap<PathBuf, Arc<Vec<SkillCatalogEntry>>>,
    sources: BTreeMap<RuntimeSourceId, RuntimeSource>,
    masks: SnapshotMasks,
    schemes: SnapshotSchemes,
}

/// The sole owner and publisher of the effective tool/skill/MCP runtime view.
///
/// Candidate construction is serialized but never holds the active pointer
/// lock. Publication is one `Arc` replacement; bound-turn dispatch reads no
/// registry lock.
///
/// Besides the base snapshot the registry keeps one lazily composed snapshot
/// per published [`ScopeOverlay`] (see [`crate::catalog_scope`]). Base and
/// scope snapshots draw from one registry-wide generation counter, so every
/// published snapshot has a unique, increasing [`ConfigGeneration`].
pub struct RuntimeRegistry {
    /// Serializes every publication and holds the last allocated
    /// registry-wide generation (base and scopes).
    publication: Mutex<ConfigGeneration>,
    active: RwLock<Arc<RuntimeSnapshot>>,
    /// Published scope overlays and their composed snapshots. Locked only
    /// while `publication` is held.
    scopes: Mutex<HashMap<ScopeKey, ScopeEntry>>,
    agent_model_preferences: AgentModelPreferences,
    agent_model_configuration: AgentModelConfigurations,
    /// `agents.<id>.reasoning` from the user's configuration files (stable
    /// Agent id → default effort). Read live per request, not per binding.
    agent_effort_configuration: watch::Sender<Arc<BTreeMap<String, ReasoningEffort>>>,
    /// `--pure`: bind_turn keeps the embedded builtin skills only and never
    /// reads external skill directories.
    pure_skills: bool,
    source_owners: Mutex<Vec<Weak<dyn RuntimeSourceOwner>>>,
}

/// One published scope overlay and its composed snapshot.
struct ScopeEntry {
    overlay: Arc<ScopeOverlay>,
    project_bundle_dirs: Arc<BTreeMap<String, PathBuf>>,
    /// Base generation `snapshot` was composed from; a different active base
    /// generation makes the next scoped bind recompose.
    base_generation: ConfigGeneration,
    snapshot: Option<Arc<RuntimeSnapshot>>,
    last_binding: Weak<RuntimeSnapshot>,
}

/// Generation-tagged view of all effective sources.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeEffectiveManifest {
    /// Config generation of the active snapshot.
    pub generation: ConfigGeneration,
    /// Sources keyed by id.
    pub sources: BTreeMap<RuntimeSourceId, RuntimeSourceManifest>,
}

/// Generation-tagged view of the published scheme table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeEffectiveSchemes {
    /// Config generation of the active snapshot.
    pub generation: ConfigGeneration,
    /// Registered external URI scheme → the winning source's binding.
    pub schemes: BTreeMap<String, SchemeBinding>,
}

/// Failure publishing a new runtime candidate.
#[derive(Clone, Debug, Error)]
pub enum RuntimeRefreshError {
    /// Tool or alias name collision.
    #[error(transparent)]
    DuplicateTool(#[from] DuplicateName),
    /// One or more contributed source names violate the naming rules; the
    /// report lists every conflict grouped by source so a single publication
    /// attempt surfaces all of them.
    #[error("contributed source names rejected:\n{0}")]
    NamingConflicts(String),
    /// One or more contributed source schema claims are invalid; the report
    /// lists every violation grouped by source so a single publication attempt
    /// surfaces all of them.
    #[error("contributed source schemas rejected:\n{0}")]
    SchemaConflicts(String),
    /// Config generation counter overflowed.
    #[error("configuration generation exhausted")]
    GenerationExhausted,
    /// Candidate failed structural validation.
    #[error("invalid runtime candidate: {0}")]
    InvalidCandidate(String),
    /// A scope overlay did not compose over the base snapshot. The base and
    /// the scope's previous snapshot are unchanged.
    #[error("catalog scope {scope} did not compose: {source}")]
    ScopeCompose {
        /// The scope key, rendered (`global`, `directory:<path>`,
        /// `project:<id>`).
        scope: String,
        /// Why the composed candidate was rejected.
        #[source]
        source: Box<RuntimeRefreshError>,
    },
}

impl RuntimeRegistry {
    /// Start a registry from a builder tools map and bundle catalog.
    #[must_use]
    pub fn new(tools: ToolRegistry, catalog: Arc<AgentCatalog>) -> Self {
        Self::from_snapshot(tools.snapshot(), catalog)
    }

    /// `--pure` mode: builtin skills only, no external skill directories.
    #[must_use]
    pub fn with_pure_skills(mut self, pure: bool) -> Self {
        self.pure_skills = pure;
        self
    }

    /// Start a registry from a frozen tool snapshot.
    #[must_use]
    pub fn from_snapshot(tools: ToolRegistrySnapshot, catalog: Arc<AgentCatalog>) -> Self {
        Self {
            publication: Mutex::new(ConfigGeneration::INITIAL),
            scopes: Mutex::new(HashMap::new()),
            active: RwLock::new(Arc::new(RuntimeSnapshot {
                generation: ConfigGeneration::INITIAL,
                catalog,
                basic_tools: tools.clone(),
                tools,
                skills: BTreeMap::new(),
                sources: BTreeMap::new(),
                masks: Arc::new(BTreeMap::new()),
                schemes: Arc::new(BTreeMap::new()),
            })),
            agent_model_preferences: watch::Sender::new(Arc::new(BTreeMap::new())),
            agent_model_configuration: watch::Sender::new(Arc::new(
                AgentModelConfiguration::default(),
            )),
            agent_effort_configuration: watch::Sender::new(Arc::new(BTreeMap::new())),
            pure_skills: false,
            source_owners: Mutex::new(Vec::new()),
        }
    }

    fn remember_source_owners<'a>(&self, sources: impl Iterator<Item = &'a RuntimeSource>) {
        let mut owners = self
            .source_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        owners.retain(|owner| owner.strong_count() > 0);
        for source in sources {
            let owner = Arc::downgrade(&source.owner);
            if !owners.iter().any(|known| known.ptr_eq(&owner)) {
                owners.push(owner);
            }
        }
    }

    /// Stop and join current and retired source connections retained by bindings.
    /// Call after composition intake and resident activations have stopped.
    pub async fn shutdown_sources(&self) {
        let owners = self
            .source_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        for owner in owners {
            owner.shutdown().await;
        }
    }

    /// Capture the complete view for one admitted turn. Skill discovery is
    /// performed once before capture; a logically unchanged result is a no-op.
    ///
    /// Same as [`Self::bind_scoped`] with [`CatalogScope::Directory`] of
    /// `workdir`.
    pub fn bind_turn(&self, workdir: &Path) -> Result<TurnBinding, RuntimeRefreshError> {
        self.bind_scoped(&CatalogScope::Directory(workdir.to_path_buf()), workdir)
    }

    /// Capture the view of `scope` for one turn in `workdir`.
    ///
    /// Skills are discovered for `workdir` (user skills only when `workdir`
    /// is empty) and published into the base keyed by `workdir`, as
    /// [`Self::bind_turn`] does. When `scope`'s key has a published
    /// [`ScopeOverlay`], the binding retains that scope's composed snapshot,
    /// recomposed first when the base generation moved since it was built;
    /// otherwise the binding retains the base snapshot.
    ///
    /// # Errors
    ///
    /// [`RuntimeRefreshError::ScopeCompose`] when the overlay no longer
    /// composes over the current base (the scope keeps its previous
    /// snapshot for retained bindings), or a base skill publication error.
    pub fn bind_scoped(
        &self,
        scope: &CatalogScope,
        workdir: &Path,
    ) -> Result<TurnBinding, RuntimeRefreshError> {
        if workdir.as_os_str().is_empty() {
            self.bind_scoped_with_skills(
                scope,
                workdir,
                hya_tool::discover_user_skills_with_builtins,
            )
        } else {
            self.bind_scoped_with_skills(scope, workdir, || discover_skills_with_builtins(workdir))
        }
    }

    /// [`Self::bind_scoped`] with caller-supplied skill discovery for
    /// `workdir` (for example a multi-root Project discovery). `discover` is
    /// not called in `--pure` mode.
    ///
    /// # Errors
    ///
    /// See [`Self::bind_scoped`].
    pub fn bind_scoped_with_skills(
        &self,
        scope: &CatalogScope,
        workdir: &Path,
        discover: impl FnOnce() -> Vec<SkillCatalogEntry>,
    ) -> Result<TurnBinding, RuntimeRefreshError> {
        let discovered = if self.pure_skills {
            hya_tool::merge_skill_catalog(Vec::new())
        } else {
            discover()
        };
        let mut last_generation = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut base = self.active();
        let agent_model_preferences = self.agent_model_preferences.borrow().clone();
        let mut agent_model_configuration = self.agent_model_configuration.borrow().clone();
        let mut retired = Vec::new();
        let existing = base
            .skills
            .get(workdir)
            .map_or(&[][..], |skills| skills.as_slice());
        if existing != discovered {
            let mut candidate = RuntimeCandidate::from_snapshot(&base);
            candidate.replace_skills(workdir, discovered);
            base = self.publish_candidate(&mut last_generation, &base, candidate, &mut retired)?;
        }

        let key = scope.key();
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut replaced = None;
        let (snapshot, project_bundle_dirs, bundle_efforts) = match scopes.get_mut(&key) {
            None => (base, Arc::new(BTreeMap::new()), Arc::new(BTreeMap::new())),
            Some(entry) => {
                let snapshot = match &entry.snapshot {
                    Some(snapshot) if entry.base_generation == base.generation => {
                        Arc::clone(snapshot)
                    }
                    _ => {
                        let composed =
                            compose_scope(&mut last_generation, &base, &entry.overlay, &key)?;
                        entry.last_binding = Arc::downgrade(&composed);
                        replaced = entry.snapshot.replace(Arc::clone(&composed));
                        entry.base_generation = base.generation;
                        composed
                    }
                };
                agent_model_configuration =
                    overlay_agent_model_configuration(agent_model_configuration, &entry.overlay);
                (
                    snapshot,
                    Arc::clone(&entry.project_bundle_dirs),
                    Arc::new(entry.overlay.bundle_efforts.clone()),
                )
            }
        };
        drop(scopes);
        // Source destruction may clean up staged files.
        drop(last_generation);
        drop(retired);
        drop(replaced);
        Ok(TurnBinding {
            snapshot,
            agent_model_preferences,
            agent_model_configuration,
            session_agent_models: Arc::new(BTreeMap::new()),
            place: Arc::new(BindingPlace {
                workdir: workdir.to_path_buf(),
                scope: scope.clone(),
                project_bundle_dirs,
                bundle_efforts,
            }),
        })
    }

    /// Publish (or replace) the overlay of scope `key` and compose its
    /// snapshot over the current base.
    ///
    /// Always replaces a previous overlay of `key`; compare
    /// [`ScopeOverlay::fingerprint`] through [`Self::scope_overlay`] first
    /// to skip an unchanged rebuild. Existing bindings keep their snapshot.
    ///
    /// # Returns
    ///
    /// The composed scope snapshot's generation (registry-wide unique).
    ///
    /// # Errors
    ///
    /// [`RuntimeRefreshError::ScopeCompose`] when the overlay fails the
    /// publication validation over the base; the base and the scope's
    /// previous overlay and snapshot stay unchanged and no generation is
    /// consumed.
    pub fn publish_scope(
        &self,
        key: ScopeKey,
        overlay: ScopeOverlay,
    ) -> Result<ConfigGeneration, RuntimeRefreshError> {
        let mut last_generation = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let base = self.active();
        let snapshot = compose_scope(&mut last_generation, &base, &overlay, &key)?;
        self.remember_source_owners(snapshot.sources.values());
        let generation = snapshot.generation;
        let entry = ScopeEntry {
            project_bundle_dirs: Arc::new(overlay.project_bundle_dirs.clone()),
            overlay: Arc::new(overlay),
            base_generation: base.generation,
            last_binding: Arc::downgrade(&snapshot),
            snapshot: Some(snapshot),
        };
        let previous = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, entry);
        drop(last_generation);
        drop(previous);
        Ok(generation)
    }

    /// Forget the overlay of scope `key`. Later binds of that scope retain
    /// the base; existing bindings keep their snapshot (and its source
    /// owners) until they drop.
    pub fn drop_scope(&self, key: &ScopeKey) {
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let removed = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key);
        drop(_publication);
        drop(removed);
    }

    /// The published overlay of scope `key`, if any.
    #[must_use]
    pub fn scope_overlay(&self, key: &ScopeKey) -> Option<Arc<ScopeOverlay>> {
        self.scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .map(|entry| Arc::clone(&entry.overlay))
    }

    /// Whether a live [`TurnBinding`] still retains the composed snapshot
    /// of scope `key` (a turn of that scope is in flight). `false` when the
    /// scope has no overlay.
    #[must_use]
    pub fn scope_in_use(&self, key: &ScopeKey) -> bool {
        self.scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .is_some_and(|entry| {
                entry.snapshot.as_ref().map_or_else(
                    || entry.last_binding.strong_count() > 0,
                    |snapshot| Arc::strong_count(snapshot) > 1,
                )
            })
    }

    /// Keys of every published scope overlay, sorted.
    #[must_use]
    pub fn scope_keys(&self) -> Vec<ScopeKey> {
        let mut keys = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        keys
    }

    /// Publish a replacement map of remembered Agent model preferences.
    ///
    /// The replacement is held independently from runtime candidates and does
    /// not allocate or consume a runtime configuration generation. Existing
    /// turn bindings retain their captured map; only later bindings observe the
    /// replacement.
    ///
    /// # Arguments
    ///
    /// * `preferences` - Complete stable-Agent-id to exact-model map to retain.
    ///
    /// # Returns
    ///
    /// This method returns `()` because Tokio watch replacement is infallible;
    /// the supplied map is always retained as the next process-local snapshot.
    pub fn publish_agent_model_preferences(&self, preferences: BTreeMap<String, ModelRef>) {
        self.agent_model_preferences
            .send_replace(Arc::new(preferences));
    }

    /// Publish a complete immutable user-file Agent model configuration.
    ///
    /// The configuration snapshot is independent from remembered preferences
    /// and runtime generations. Existing bindings retain their captured
    /// configuration; only bindings created afterwards observe this map.
    pub fn publish_agent_model_configuration(&self, configuration: AgentModelConfiguration) {
        self.agent_model_configuration
            .send_replace(Arc::new(configuration));
    }

    /// Publish the complete `agents.<id>.reasoning` map. Unlike model
    /// configuration it is read at request time, so it applies to the next
    /// request of every session.
    pub fn publish_agent_effort_configuration(&self, efforts: BTreeMap<String, ReasoningEffort>) {
        self.agent_effort_configuration
            .send_replace(Arc::new(efforts));
    }

    /// The published `agents.<id>.reasoning` map.
    #[must_use]
    pub fn agent_effort_configuration(&self) -> Arc<BTreeMap<String, ReasoningEffort>> {
        Arc::clone(&self.agent_effort_configuration.borrow())
    }

    /// Build and validate a complete candidate, then publish it with one pointer
    /// replacement. Failed candidates do not allocate a generation. `build`
    /// applies prepared in-memory inputs; callers perform discovery/I/O first.
    pub fn refresh(
        &self,
        build: impl FnOnce(&mut RuntimeCandidate) -> Result<(), RuntimeRefreshError>,
    ) -> Result<ConfigGeneration, RuntimeRefreshError> {
        let mut last_generation = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = self.active();
        let mut candidate = RuntimeCandidate::from_snapshot(&current);
        if let Err(error) = build(&mut candidate) {
            drop(last_generation);
            return Err(error);
        }
        if candidate.logically_matches(&current) {
            drop(last_generation);
            return Ok(current.generation);
        }
        let mut retired = Vec::new();
        let published =
            self.publish_candidate(&mut last_generation, &current, candidate, &mut retired)?;
        drop(last_generation);
        drop(retired);
        Ok(published.generation)
    }

    /// Atomically publish a complete agent catalog while preserving the
    /// current tool, skill, and source view.
    pub fn publish_catalog(
        &self,
        catalog: Arc<AgentCatalog>,
    ) -> Result<ConfigGeneration, RuntimeRefreshError> {
        let mut last_generation = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = self.active();
        if current.catalog.bundles().bundles() == catalog.bundles().bundles() {
            return Ok(current.generation);
        }
        let generation = allocate_generation(&mut last_generation)?;
        let published = Arc::new(RuntimeSnapshot {
            generation,
            catalog,
            basic_tools: current.basic_tools.clone(),
            tools: current.tools.clone(),
            skills: current.skills.clone(),
            sources: current.sources.clone(),
            masks: Arc::clone(&current.masks),
            schemes: Arc::clone(&current.schemes),
        });
        *self
            .active
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = published;
        Ok(generation)
    }

    #[must_use]
    /// Model-facing tool schemas from this snapshot or view.
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.active().tools.schemas()
    }

    /// The API endpoints `bundle_id` serves in the live generation.
    ///
    /// `None` when the bundle is not in the published catalog or declares no
    /// endpoints. The returned handle retains that generation's process.
    #[must_use]
    pub fn bundle_apis(&self, bundle_id: &str) -> Option<crate::bundle_apis::SourceApis> {
        snapshot_bundle_apis(&self.active(), bundle_id)
    }

    /// Every published bundle's declared endpoints, sorted by bundle id.
    #[must_use]
    pub fn published_bundle_apis(&self) -> Vec<crate::bundle_apis::PublishedBundleApis> {
        snapshot_published_bundle_apis(&self.active())
    }

    /// Every published bundle's declared session permission modes, sorted
    /// by bundle id then mode id.
    #[must_use]
    pub fn published_permission_modes(
        &self,
    ) -> Vec<crate::permission_mode::PublishedPermissionMode> {
        let active = self.active();
        published_permission_modes(&active)
    }

    #[must_use]
    /// Generation-tagged source manifests for diagnostics.
    pub fn effective_manifest(&self) -> RuntimeEffectiveManifest {
        let active = self.active();
        RuntimeEffectiveManifest {
            generation: active.generation,
            sources: active
                .sources
                .iter()
                .map(|(id, source)| {
                    (
                        id.clone(),
                        RuntimeSourceManifest {
                            id: id.clone(),
                            declaration_digest: source.declaration_digest,
                            exports: source
                                .exports
                                .iter()
                                .map(|export| export.canonical_name.clone())
                                .collect(),
                            skills: source
                                .skills
                                .iter()
                                .map(|skill| skill.stable_id.clone())
                                .collect(),
                            skill_entries: source
                                .skills
                                .iter()
                                .map(|skill| skill.entry.clone())
                                .collect(),
                            resources: source.resources.clone(),
                        },
                    )
                })
                .collect(),
        }
    }

    /// Last publication across the base and every scope, including partial
    /// refreshes whose other sources failed.
    #[must_use]
    pub fn publication_generation(&self) -> ConfigGeneration {
        *self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Bundle sources of the last base publication, for composing Project
    /// catalogs without preparing unrelated running sources again.
    #[must_use]
    pub fn published_bundle_sources(&self) -> Vec<RuntimeSource> {
        self.active()
            .sources
            .values()
            .filter(|source| source.id.kind() == RuntimeSourceKind::Bundle)
            .cloned()
            .collect()
    }

    /// Current user-global config plugin hooks. Idle sessions never retain
    /// these dispatchers; a config removal releases them at publication.
    #[must_use]
    pub fn global_plugin_hooks(&self) -> Vec<Arc<dyn crate::hooks::HookDispatcher>> {
        self.active()
            .sources
            .values()
            .filter(|source| source.id.kind() == RuntimeSourceKind::Plugin)
            .filter_map(|source| source.hooks.clone())
            .collect()
    }

    fn active(&self) -> Arc<RuntimeSnapshot> {
        self.active
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    /// Generation-tagged scheme table for diagnostics/UI.
    pub fn effective_schemes(&self) -> RuntimeEffectiveSchemes {
        let active = self.active();
        RuntimeEffectiveSchemes {
            generation: active.generation,
            schemes: active.schemes.as_ref().clone(),
        }
    }

    /// Every claimant of `scheme` in the active snapshot, ordered by ascending
    /// source id with the active provider last. See
    /// [`TurnBinding::scheme_chain`] for the binding-level accessor.
    #[must_use]
    pub fn scheme_chain(&self, scheme: &str) -> Vec<(String, String)> {
        self.active()
            .sources
            .values()
            .filter_map(|source| {
                let claim = source.schemas.iter().find(|claim| claim.scheme == scheme)?;
                Some((source.id.to_string(), claim.canonical_tool.clone()))
            })
            .collect()
    }

    fn publish_candidate(
        &self,
        last_generation: &mut ConfigGeneration,
        current: &RuntimeSnapshot,
        candidate: RuntimeCandidate,
        retired: &mut Vec<Arc<RuntimeSnapshot>>,
    ) -> Result<Arc<RuntimeSnapshot>, RuntimeRefreshError> {
        let published = Arc::new(candidate.into_snapshot(
            allocate_generation(last_generation)?,
            current.basic_tools.clone(),
        ));
        self.remember_source_owners(published.sources.values());
        *self
            .active
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = published.clone();
        // Cached compositions must not pin retired global sources. The
        // overlay and live turn bindings survive; the next bind recomposes.
        retired.extend(
            self.scopes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values_mut()
                .filter_map(|entry| entry.snapshot.take()),
        );
        Ok(published)
    }
}

/// Allocate the next registry-wide generation. Callers hold the publication
/// lock, which owns `last`.
fn allocate_generation(
    last: &mut ConfigGeneration,
) -> Result<ConfigGeneration, RuntimeRefreshError> {
    let next = last
        .checked_next()
        .ok_or(RuntimeRefreshError::GenerationExhausted)?;
    *last = next;
    Ok(next)
}

/// Compose `overlay` over `base` into a new scope snapshot: the overlay's
/// catalog, its Bundle sources in place of the base's, and its Plugin sources
/// the base does not already publish. Validation is the base publication's;
/// a failure consumes no generation.
fn compose_scope(
    last_generation: &mut ConfigGeneration,
    base: &RuntimeSnapshot,
    overlay: &ScopeOverlay,
    key: &ScopeKey,
) -> Result<Arc<RuntimeSnapshot>, RuntimeRefreshError> {
    let failed = |source: RuntimeRefreshError| RuntimeRefreshError::ScopeCompose {
        scope: key.to_string(),
        source: Box::new(source),
    };
    if let Some(source) = overlay
        .plugin_sources
        .iter()
        .find(|source| source.id.kind() != RuntimeSourceKind::Plugin)
    {
        return Err(failed(RuntimeRefreshError::InvalidCandidate(format!(
            "scope plugin source {} is not a Plugin source",
            source.id
        ))));
    }
    let mut candidate = RuntimeCandidate::from_snapshot(base);
    candidate.replace_catalog(Arc::clone(&overlay.catalog));
    candidate
        .replace_sources_of_kind(RuntimeSourceKind::Bundle, overlay.bundle_sources.clone())
        .map_err(failed)?;
    let plugins = overlay
        .plugin_sources
        .iter()
        .filter(|source| !base.sources.contains_key(&source.id))
        .cloned()
        .map(|mut source| {
            source.project_plugin = true;
            source
        })
        .collect::<Vec<_>>();
    if !plugins.is_empty() {
        candidate.upsert_sources(plugins).map_err(failed)?;
    }
    let generation = allocate_generation(last_generation).map_err(failed)?;
    Ok(Arc::new(
        candidate.into_snapshot(generation, base.basic_tools.clone()),
    ))
}

/// The base model configuration as seen by one scope: scope bundle ids drop
/// their user-scope leaves (the project bundle shadows that install) and the
/// overlay's own leaves apply.
fn overlay_agent_model_configuration(
    base: AgentModelConfigurationSnapshot,
    overlay: &ScopeOverlay,
) -> AgentModelConfigurationSnapshot {
    let shadows = overlay
        .project_bundle_dirs
        .keys()
        .chain(overlay.bundle_models.keys())
        .any(|bundle_id| base.bundles.contains_key(bundle_id));
    if !shadows && overlay.bundle_models.is_empty() {
        return base;
    }
    let mut configuration = base.as_ref().clone();
    for bundle_id in overlay.project_bundle_dirs.keys() {
        configuration.bundles.remove(bundle_id);
    }
    for (bundle_id, models) in &overlay.bundle_models {
        if models.is_empty() {
            configuration.bundles.remove(bundle_id);
        } else {
            configuration
                .bundles
                .insert(bundle_id.clone(), models.clone());
        }
    }
    Arc::new(configuration)
}

fn snapshot_bundle_apis(
    snapshot: &RuntimeSnapshot,
    bundle_id: &str,
) -> Option<crate::bundle_apis::SourceApis> {
    if !snapshot
        .catalog
        .bundles()
        .bundles()
        .iter()
        .any(|bundle| bundle.identity().id == bundle_id)
    {
        return None;
    }
    snapshot
        .sources
        .get(&RuntimeSourceId::bundle(bundle_id))?
        .apis
        .clone()
}

fn snapshot_published_bundle_apis(
    snapshot: &RuntimeSnapshot,
) -> Vec<crate::bundle_apis::PublishedBundleApis> {
    snapshot
        .sources
        .iter()
        .filter(|(id, _)| id.kind() == RuntimeSourceKind::Bundle)
        .filter(|(id, _)| {
            snapshot
                .catalog
                .bundles()
                .bundles()
                .iter()
                .any(|bundle| bundle.identity().id == id.configured_id())
        })
        .filter_map(|(id, source)| {
            source
                .apis
                .as_ref()
                .map(|apis| crate::bundle_apis::PublishedBundleApis {
                    bundle: id.configured_id().to_string(),
                    apis: apis.apis.clone(),
                })
        })
        .collect()
}

fn published_permission_modes(
    snapshot: &RuntimeSnapshot,
) -> Vec<crate::permission_mode::PublishedPermissionMode> {
    snapshot
        .sources
        .iter()
        .filter(|(id, _)| id.kind() == RuntimeSourceKind::Bundle)
        .filter(|(id, _)| {
            snapshot
                .catalog
                .bundles()
                .bundles()
                .iter()
                .any(|bundle| bundle.identity().id == id.configured_id())
        })
        .flat_map(|(id, source)| {
            let bundle = id.configured_id();
            source.permission_modes.iter().map(move |mode| {
                crate::permission_mode::PublishedPermissionMode {
                    id: format!("{bundle}/{}", mode.id),
                    title: mode.title.clone(),
                    description: mode.description.clone(),
                    source: bundle.to_string(),
                }
            })
        })
        .collect()
}
