use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use hya_bundle::{BundleCatalog, BundleError, ExportKind, ResourceView};

use crate::agent_catalog::{AgentCatalog, AgentDefinition, AgentOrigin};
use crate::catalog_scope::{CatalogScope, ScopeKey, ScopeOverlay};
use hya_proto::{ConfigGeneration, ModelRef, ToolName, ToolSchema};
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

const RUNTIME_SOURCE_DISPATCH_IDENTITY_DOMAIN_V1: &[u8] = b"hya.core.runtime-source-dispatch/v1";
const RUNTIME_SEMANTIC_FINGERPRINT_DOMAIN_V2: &[u8] = b"hya.core.runtime-semantic-fingerprint/v2";

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
    /// `--pure`: bind_turn keeps the embedded builtin skills only and never
    /// reads external skill directories.
    pure_skills: bool,
}

/// One published scope overlay and its composed snapshot.
struct ScopeEntry {
    overlay: Arc<ScopeOverlay>,
    project_bundle_dirs: Arc<BTreeMap<String, PathBuf>>,
    /// Base generation `snapshot` was composed from; a different active base
    /// generation makes the next scoped bind recompose.
    base_generation: ConfigGeneration,
    snapshot: Arc<RuntimeSnapshot>,
}

/// Offline mutable candidate. Its contents cannot become effective except
/// through [`RuntimeRegistry::refresh`].
pub struct RuntimeCandidate {
    catalog: Arc<AgentCatalog>,
    tools: ToolRegistry,
    skills: BTreeMap<PathBuf, Arc<Vec<SkillCatalogEntry>>>,
    sources: BTreeMap<RuntimeSourceId, RuntimeSource>,
    masks: BTreeMap<String, String>,
    schemes: BTreeMap<String, SchemeBinding>,
}

/// Kind of runtime contribution source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeSourceKind {
    /// Prepared bundle resources and their generation-owned process providers.
    Bundle,
    /// MCP server tools.
    Mcp,
    /// Plugin-declared tools and Skills.
    Plugin,
}

/// Stable identity of a configured bundle, MCP, or plugin source.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeSourceId {
    kind: RuntimeSourceKind,
    configured_id: String,
}

/// Marker for process-owned source handles retained by the registry.
///
/// **Contract:** Implementors keep clients/processes alive while the
/// [`RuntimeSource`] is published. No methods — ownership is the contract.
pub trait RuntimeSourceOwner: Send + Sync {}

impl<T: Send + Sync> RuntimeSourceOwner for T {}

/// One tool export from a runtime source (canonical name + aliases).
#[derive(Clone)]
pub struct RuntimeSourceExport {
    declared_id: String,
    canonical_name: String,
    aliases: Vec<String>,
    tool: Arc<dyn Tool>,
    permission: ToolPermission,
}

/// Published bundle/MCP/plugin source with tools, Skills, and opaque resources.
#[derive(Clone)]
pub struct RuntimeSource {
    id: RuntimeSourceId,
    declaration_digest: [u8; 32],
    owner: Arc<dyn RuntimeSourceOwner>,
    exports: Vec<RuntimeSourceExport>,
    skills: Vec<RuntimeSourceSkill>,
    resources: Arc<BTreeMap<String, Value>>,
    schemas: Vec<SourceSchema>,
    hooks: Option<Arc<dyn crate::hooks::HookDispatcher>>,
    apis: Option<crate::bundle_apis::SourceApis>,
    permission_modes: Vec<crate::permission_mode::RuntimePermissionMode>,
}

/// One external URI-scheme claim a runtime source makes.
///
/// `canonical_tool` must be a canonical export of the *same* source — except
/// for `Bundle` sources, whose tools are view-scoped (sidecar activation)
/// rather than registry exports, so a bundle claims its tool by the
/// `bundle:{id}/tool/{local}` stable id even though it exports no registry
/// tools. `scheme` must be a publishable token; both rules are enforced at
/// publication. Claims are adjudicated across sources the way bare-name
/// aliases are: the lexicographically greater source id wins and the table
/// records only the winner, with the full claim chain still derivable through
/// [`TurnBinding::scheme_chain`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSchema {
    /// The external URI scheme (e.g. `db` for `db://x/y`).
    pub scheme: String,
    /// Canonical name of this source's tool that serves the scheme.
    pub canonical_tool: String,
    /// Whether the scheme accepts write dispatch in addition to reads.
    pub writable: bool,
}

/// One Skill contribution materialized for a published runtime source.
///
/// `stable_id` is the resource identity used by bundle resource views. The
/// parsed entry carries the complete Skill metadata and a deterministic virtual
/// path; no runtime consumer reparses the original Markdown.
#[derive(Clone)]
pub struct RuntimeSourceSkill {
    stable_id: String,
    local_id: String,
    aliases: Vec<String>,
    digest: String,
    content: String,
    entry: SkillCatalogEntry,
}

/// Serialisable summary of a source for diagnostics/UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSourceManifest {
    /// Source identity.
    pub id: RuntimeSourceId,
    /// Digest of the source declaration.
    pub declaration_digest: [u8; 32],
    /// Canonical export names.
    pub exports: Vec<String>,
    /// Canonical Skill resource ids.
    pub skills: Vec<String>,
    /// Parsed Skill metadata retained by the published source.
    pub skill_entries: Vec<SkillCatalogEntry>,
    /// Opaque resource map from the source.
    pub resources: Arc<BTreeMap<String, Value>>,
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

/// One admitted turn's immutable runtime binding.
///
/// Keep every field behind an `Arc` (place data goes in [`BindingPlace`]):
/// bindings are moved by value through deep async state machines, and a
/// larger binding has overflowed the stack before.
#[derive(Clone)]
pub struct TurnBinding {
    snapshot: Arc<RuntimeSnapshot>,
    agent_model_preferences: AgentModelPreferenceSnapshot,
    agent_model_configuration: AgentModelConfigurationSnapshot,
    session_agent_models: AgentModelPreferenceSnapshot,
    place: Arc<BindingPlace>,
}

/// Where a [`TurnBinding`] was bound: one `Arc` keeps the binding small.
struct BindingPlace {
    workdir: PathBuf,
    scope: CatalogScope,
    project_bundle_dirs: Arc<BTreeMap<String, PathBuf>>,
}

#[cfg(test)]
impl BindingPlace {
    fn new(workdir: PathBuf, scope: CatalogScope) -> Arc<Self> {
        Arc::new(Self {
            workdir,
            scope,
            project_bundle_dirs: Arc::new(BTreeMap::new()),
        })
    }
}

/// Which Harness-owned resources a bound agent may see.
///
/// **Derived from [`AgentOrigin`], never from a manifest.** A bundle author
/// cannot widen their own plane; that is the whole point of the clamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentToolPlane {
    /// Built-in agents: the live tool snapshot, Harness skills, and Harness MCP.
    Full,
    /// Bundle agents: the internal public tool snapshot captured when the
    /// registry was built. No Harness skills, no Harness MCP, no plugin tools.
    InternalPublic,
}

impl AgentToolPlane {
    /// Plane for an agent of this origin.
    #[must_use]
    pub const fn for_origin(origin: &AgentOrigin<'_>) -> Self {
        match origin {
            AgentOrigin::Builtin => Self::Full,
            AgentOrigin::Bundle { .. } => Self::InternalPublic,
        }
    }

    /// Short label used in diagnostics and plane-violation errors.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::InternalPublic => "internal-public",
        }
    }
}

/// Catalog-derived policy retained only for one in-process agent execution.
/// It contains no agent identity and is never persisted or exposed on the wire.
#[derive(Clone, Debug)]
pub struct AgentResourcePolicy {
    /// Owning bundle id, or `None` for a built-in agent (which owns no bundle).
    bundle_id: Option<String>,
    plane: AgentToolPlane,
    resource_view: ResourceView,
    /// Whether the agent may spawn anyone (a non-empty effective spawn set);
    /// decides the `task`/`archive` coordination tools.
    spawn_rights: bool,
    selected_bundle_tool_ids: Arc<Vec<String>>,
    selected_bundle_skill_ids: Arc<Vec<String>>,
    canonical_hook_ids: Arc<[String]>,
}

impl AgentResourcePolicy {
    /// Bundle-local tool ids selected for this agent view.
    #[must_use]
    pub fn selected_bundle_tool_ids(&self) -> &[String] {
        self.selected_bundle_tool_ids.as_slice()
    }

    /// Bundle-local Skill ids selected for this agent view.
    #[must_use]
    pub fn selected_bundle_skill_ids(&self) -> &[String] {
        self.selected_bundle_skill_ids.as_slice()
    }

    /// Canonical hook ids activated for this agent view.
    #[must_use]
    pub fn canonical_hook_ids(&self) -> &[String] {
        self.canonical_hook_ids.as_ref()
    }

    /// Harness resource plane this agent is bound to.
    #[must_use]
    pub fn plane(&self) -> AgentToolPlane {
        self.plane
    }

    /// Owning bundle id, or `None` for a built-in agent.
    #[must_use]
    pub fn bundle_id(&self) -> Option<&str> {
        self.bundle_id.as_deref()
    }

    /// Namespace used for bundle-local qualified public names.
    ///
    /// Built-ins have no bundle namespace; harness candidates keep their
    /// `harness:<kind>/<name>` spelling regardless.
    fn namespace(&self) -> &str {
        self.resource_view
            .namespace
            .as_deref()
            .or(self.bundle_id.as_deref())
            .unwrap_or("harness")
    }

    /// Scope label used in resource-resolution error messages.
    fn diagnostic_scope(&self) -> &str {
        self.bundle_id.as_deref().unwrap_or("builtin")
    }
}

/// Immutable per-turn/child resource map compiled once from a retained
/// [`TurnBinding`] and bound agent policy. Schema visibility, skill exposure,
/// and dispatch share this map; there is no registry fallback.
pub(crate) struct CompiledResourceView {
    tools: BTreeMap<String, ResolvedTool>,
    schemas: Vec<ToolSchema>,
    skills: Arc<Vec<SkillCatalogEntry>>,
    canonical_hook_ids: Arc<[String]>,
    /// Whether the selected view includes the canonical harness skill facade
    /// tool (regardless of any public alias spelling for that tool).
    skill_facade_selected: bool,
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
            pure_skills: false,
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

    /// Capture a view with no project: user skills and builtins only.
    ///
    /// For catalog listings whose request names no directory (`hya serve`
    /// has no working directory, ADR-0024). The binding's workdir is the
    /// empty path; it keys the project-less skill set and is never a turn's
    /// workdir. Same as [`Self::bind_scoped`] with [`CatalogScope::Global`].
    pub fn bind_global(&self) -> Result<TurnBinding, RuntimeRefreshError> {
        self.bind_scoped(&CatalogScope::Global, Path::new(""))
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
        let mut last_generation = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut base = self.active();
        let agent_model_preferences = self.agent_model_preferences.borrow().clone();
        let mut agent_model_configuration = self.agent_model_configuration.borrow().clone();
        let discovered = if self.pure_skills {
            hya_tool::merge_skill_catalog(Vec::new())
        } else {
            discover()
        };
        let existing = base
            .skills
            .get(workdir)
            .map_or(&[][..], |skills| skills.as_slice());
        if existing != discovered {
            let mut candidate = RuntimeCandidate::from_snapshot(&base);
            candidate.replace_skills(workdir, discovered);
            base = self.publish_candidate(&mut last_generation, &base, candidate)?;
        }

        let key = scope.key();
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut replaced = None;
        let (snapshot, project_bundle_dirs) = match scopes.get_mut(&key) {
            None => (base, Arc::new(BTreeMap::new())),
            Some(entry) => {
                if entry.base_generation != base.generation {
                    let composed =
                        compose_scope(&mut last_generation, &base, &entry.overlay, &key)?;
                    replaced = Some(std::mem::replace(&mut entry.snapshot, composed));
                    entry.base_generation = base.generation;
                }
                agent_model_configuration =
                    overlay_agent_model_configuration(agent_model_configuration, &entry.overlay);
                (
                    Arc::clone(&entry.snapshot),
                    Arc::clone(&entry.project_bundle_dirs),
                )
            }
        };
        drop(scopes);
        // Release a superseded scope snapshot's sources outside the lock.
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
        let generation = snapshot.generation;
        let entry = ScopeEntry {
            project_bundle_dirs: Arc::new(overlay.project_bundle_dirs.clone()),
            overlay: Arc::new(overlay),
            base_generation: base.generation,
            snapshot,
        };
        let previous = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, entry);
        // Release the replaced scope's sources outside the scope lock.
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

    /// Build and validate a complete candidate, then publish it with one pointer
    /// replacement. Failed candidates do not allocate a generation.
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
        build(&mut candidate)?;
        if candidate.logically_matches(&current) {
            return Ok(current.generation);
        }
        Ok(self
            .publish_candidate(&mut last_generation, &current, candidate)?
            .generation)
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
        let active = self.active();
        if !active
            .catalog
            .bundles()
            .bundles()
            .iter()
            .any(|bundle| bundle.identity().id == bundle_id)
        {
            return None;
        }
        active
            .sources
            .get(&RuntimeSourceId::bundle(bundle_id))?
            .apis
            .clone()
    }

    /// Every published bundle's declared endpoints, sorted by bundle id.
    #[must_use]
    pub fn published_bundle_apis(&self) -> Vec<crate::bundle_apis::PublishedBundleApis> {
        let active = self.active();
        active
            .sources
            .iter()
            .filter(|(id, _)| id.kind() == RuntimeSourceKind::Bundle)
            .filter(|(id, _)| {
                active
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
    ) -> Result<Arc<RuntimeSnapshot>, RuntimeRefreshError> {
        let published = Arc::new(candidate.into_snapshot(
            allocate_generation(last_generation)?,
            current.basic_tools.clone(),
        ));
        *self
            .active
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = published.clone();
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

impl RuntimeCandidate {
    fn into_snapshot(
        self,
        generation: ConfigGeneration,
        basic_tools: ToolRegistrySnapshot,
    ) -> RuntimeSnapshot {
        let Self {
            catalog,
            tools,
            skills,
            sources,
            masks,
            schemes,
        } = self;
        RuntimeSnapshot {
            generation,
            catalog,
            basic_tools,
            tools: tools.snapshot(),
            skills,
            sources,
            masks: Arc::new(masks),
            schemes: Arc::new(schemes),
        }
    }

    fn from_snapshot(snapshot: &RuntimeSnapshot) -> Self {
        Self {
            catalog: Arc::clone(&snapshot.catalog),
            tools: ToolRegistry::from_snapshot(&snapshot.tools),
            skills: snapshot.skills.clone(),
            sources: snapshot.sources.clone(),
            masks: snapshot.masks.as_ref().clone(),
            schemes: snapshot.schemes.as_ref().clone(),
        }
    }

    /// Replace the complete Agent/Bundle catalog on this offline candidate.
    pub fn replace_catalog(&mut self, catalog: Arc<AgentCatalog>) {
        self.catalog = catalog;
    }

    /// Register a tool with default `Tool` permission on this candidate.
    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) -> Result<(), RuntimeRefreshError> {
        self.register_tool_with_permission(tool, ToolPermission::Tool)
    }

    /// Register a tool with an explicit permission class on this candidate.
    pub fn register_tool_with_permission(
        &mut self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
    ) -> Result<(), RuntimeRefreshError> {
        self.tools.register_with_permission(tool, permission)?;
        Ok(())
    }

    /// Remove a tool and its aliases from this candidate.
    pub fn remove_tool(&mut self, name: &str) {
        if self.tools.resolve(name).is_some() {
            self.tools.remove(name);
        }
    }

    /// Rediscover skills for `workdir` into this candidate.
    pub fn refresh_skills(&mut self, workdir: &Path) {
        self.replace_skills(workdir, discover_skills_with_builtins(workdir));
    }

    /// Validate every contributed source export name against the naming rules
    /// before any mutation happens, and derive the bare-name mask table:
    ///
    /// - plugin sources must publish qualified `namespace__local` tool names
    ///   (bare names are reserved for built-in tools) whose namespace head is
    ///   not one of the reserved `mcp`/`harness`/`builtin` planes;
    /// - MCP sources must publish exactly `mcp__<server>__<local>` with valid
    ///   tokens;
    /// - canonical names must be unique across sources (hard error);
    /// - the protected bare name `read` can never be claimed or masked by a
    ///   contributed alias (hard error);
    /// - any other alias collision — with a built-in canonical name, another
    ///   source's canonical name, or another source's alias — becomes an
    ///   explicit mask instead of an error. A source always beats a built-in
    ///   (user scope over the built-in plane), and between two sources the
    ///   lexicographically GREATER source id string wins, which approximates
    ///   "the newer install wins". The winner's tool provides the contested
    ///   name; the loser keeps its qualified canonical spelling as the escape
    ///   hatch.
    ///
    /// All hard conflicts are reported in one grouped diagnostic. The returned
    /// map is the published mask table (contested bare name → winning
    /// canonical name); contested aliases are deliberately not registered as
    /// registry aliases, so the bare plane resolves through compiled views.
    fn validate_candidate_source_names(
        &self,
        sources: &[RuntimeSource],
    ) -> Result<BTreeMap<String, String>, RuntimeRefreshError> {
        let (conflicts, masks) = self.adjudicate_source_name_claims(sources);
        if conflicts.is_empty() {
            Ok(masks)
        } else {
            Err(RuntimeRefreshError::NamingConflicts(conflicts.join("\n")))
        }
    }

    /// One claim on a name in the global bare-name plane: `label` is the
    /// source id string (or `built-in`), `canonical` the claimant's canonical
    /// tool name.
    fn record_claim(
        claims: &mut BTreeMap<String, Vec<(String, String)>>,
        name: &str,
        label: &str,
        canonical: &str,
    ) {
        claims
            .entry(name.to_string())
            .or_default()
            .push((label.to_string(), canonical.to_string()));
    }

    /// Validate the candidate source names and adjudicate every bare-name
    /// contest into the mask table. See
    /// [`RuntimeCandidate::validate_candidate_source_names`] for the contract.
    fn adjudicate_source_name_claims(
        &self,
        sources: &[RuntimeSource],
    ) -> (Vec<String>, BTreeMap<String, String>) {
        const RESERVED_HEADS: [&str; 3] = ["mcp", "harness", "builtin"];
        const BUILTIN_LABEL: &str = "built-in";
        let valid_segment = |token: &str| {
            !token.is_empty()
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        };
        let claim_rank = |(label, _): &(String, String)| {
            let source_rank = u8::from(label != BUILTIN_LABEL);
            (source_rank, label.clone())
        };

        let replaced: BTreeSet<String> = sources.iter().map(|s| s.id.to_string()).collect();
        let mut conflicts: Vec<String> = Vec::new();
        // Canonical name → owning source label, for hard duplicate detection
        // and for identifying the built-in remainder of the bare-name plane.
        let mut canonical_owner: BTreeMap<String, String> = BTreeMap::new();
        // Contested-name claims from contributed aliases: alias → claimants.
        let mut alias_claims: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

        for (id, source) in &self.sources {
            if replaced.contains(&id.to_string()) {
                continue;
            }
            let label = id.to_string();
            for export in &source.exports {
                canonical_owner.insert(export.canonical_name.clone(), label.clone());
            }
        }

        for source in sources {
            let label = source.id.to_string();
            for export in &source.exports {
                let name = export.canonical_name.as_str();
                match source.id.kind {
                    RuntimeSourceKind::Plugin | RuntimeSourceKind::Bundle => {
                        let segments: Vec<&str> = name.split("__").collect();
                        let shaped = segments.len() >= 2
                            && segments.iter().all(|segment| valid_segment(segment))
                            && !RESERVED_HEADS.contains(&segments[0]);
                        if !shaped {
                            conflicts.push(format!(
                                "source {label}: tool name `{name}` must be qualified as \
                                 `namespace__local` with a non-reserved namespace token"
                            ));
                            continue;
                        }
                    }
                    RuntimeSourceKind::Mcp => {
                        let parts: Vec<&str> = name.split("__").collect();
                        let shaped = parts.len() == 3
                            && parts[0] == "mcp"
                            && valid_segment(parts[1])
                            && valid_segment(parts[2]);
                        if !shaped {
                            conflicts.push(format!(
                                "source {label}: MCP tool name `{name}` must be \
                                 `mcp__<server>__<local>` with valid tokens"
                            ));
                            continue;
                        }
                    }
                }
                if let Some(owner) = canonical_owner.get(name) {
                    conflicts.push(format!(
                        "source {label}: canonical name `{name}` is already provided by {owner}"
                    ));
                    continue;
                }
                canonical_owner.insert(name.to_string(), label.clone());
                for alias in &export.aliases {
                    if hya_tool::tool_bundle_presets()
                        .iter()
                        .any(|preset| preset.is_protected(alias))
                    {
                        conflicts.push(format!(
                            "source {label}: protected tool `{alias}` cannot be masked; \
                             the alias is rejected"
                        ));
                        continue;
                    }
                    if canonical_owner
                        .get(alias)
                        .is_some_and(|owner| owner == &label)
                    {
                        conflicts.push(format!(
                            "source {label}: alias `{alias}` collides with its own canonical name"
                        ));
                        continue;
                    }
                    let claimants = alias_claims.entry(alias.clone()).or_default();
                    if claimants.iter().any(|(owner, _)| owner == &label) {
                        conflicts.push(format!(
                            "source {label}: alias `{alias}` is declared more than once"
                        ));
                        continue;
                    }
                    claimants.push((label.clone(), export.canonical_name.clone()));
                }
            }
        }
        for (id, source) in &self.sources {
            if replaced.contains(&id.to_string()) {
                continue;
            }
            let label = id.to_string();
            for export in &source.exports {
                for alias in &export.aliases {
                    Self::record_claim(&mut alias_claims, alias, &label, &export.canonical_name);
                }
            }
        }

        // Adjudication: a name contested by more than one claimant resolves by
        // the masking total order — sources beat built-ins, and between two
        // sources the lexicographically greater source id string wins.
        // Canonical names owned by replaced sources still sit in the tool
        // snapshot during validation; they are about to be removed, so they
        // must not pose as built-in claims.
        let replaced_canonicals: BTreeSet<String> = self
            .sources
            .iter()
            .filter(|(id, _)| replaced.contains(&id.to_string()))
            .flat_map(|(_, source)| {
                source
                    .exports
                    .iter()
                    .map(|export| export.canonical_name.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        let tool_canonicals: BTreeSet<String> = self
            .tools
            .snapshot()
            .canonical_tools()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let mut masks = BTreeMap::new();
        for (name, mut claimants) in alias_claims {
            if let Some(owner) = canonical_owner.get(&name) {
                claimants.push((owner.clone(), name.clone()));
            } else if !replaced_canonicals.contains(&name) && tool_canonicals.contains(&name) {
                claimants.push((BUILTIN_LABEL.to_string(), name.clone()));
            }
            if claimants.len() < 2 {
                continue;
            }
            claimants.sort_by_key(claim_rank);
            if let Some((_, winner)) = claimants.last() {
                masks.insert(name, winner.clone());
            }
        }
        (conflicts, masks)
    }

    /// Recompute the mask table for the candidate's retained sources after a
    /// removal. Retained sources are pre-validated, so this cannot produce
    /// hard conflicts.
    fn recompute_masks_after_removal(&mut self) {
        let (_, masks) = self.adjudicate_source_name_claims(&[]);
        self.masks = masks;
    }

    /// Recompute the scheme table for the candidate's retained sources after a
    /// removal. Retained sources are pre-validated, so this cannot produce
    /// hard conflicts.
    fn recompute_schemes_after_removal(&mut self) {
        let (_, schemes) = self.adjudicate_source_scheme_claims(&[]);
        self.schemes = schemes;
    }

    /// Validate every contributed source schema claim before any mutation
    /// happens, and derive the published scheme table:
    ///
    /// - the scheme must be a `[a-zA-Z0-9_-]` token of at least two characters
    ///   without the `__` separator (hard error);
    /// - the internal families `artifact`/`skill`/`local` are reserved and can
    ///   never be registered (hard error);
    /// - the claimed `canonical_tool` must be a canonical export of the *same*
    ///   source (hard error), so a source can only bind schemes to tools it
    ///   actually ships. `Bundle` sources are exempt: their tools are
    ///   view-scoped (sidecar activation) rather than registry exports, so a
    ///   bundle claims its owning tool by its `bundle:{id}/tool/{local}`
    ///   stable id instead;
    /// - a scheme claimed twice by one source is a declaration bug (hard
    ///   error);
    /// - a scheme claimed by two different sources resolves by the masking
    ///   total order: the lexicographically GREATER source id string wins,
    ///   approximating "the newer install wins". The table records only the
    ///   winner; the full claim chain stays derivable through
    ///   [`TurnBinding::scheme_chain`].
    ///
    /// All hard violations are reported in one grouped diagnostic.
    fn validate_candidate_source_schemes(
        &self,
        sources: &[RuntimeSource],
    ) -> Result<BTreeMap<String, SchemeBinding>, RuntimeRefreshError> {
        let (conflicts, schemes) = self.adjudicate_source_scheme_claims(sources);
        if conflicts.is_empty() {
            Ok(schemes)
        } else {
            Err(RuntimeRefreshError::SchemaConflicts(conflicts.join("\n")))
        }
    }

    /// Collect and adjudicate every scheme claim from the candidate sources
    /// and the retained (non-replaced) sources. See
    /// [`RuntimeCandidate::validate_candidate_source_schemes`] for the contract.
    fn adjudicate_source_scheme_claims(
        &self,
        sources: &[RuntimeSource],
    ) -> (Vec<String>, BTreeMap<String, SchemeBinding>) {
        let replaced: BTreeSet<String> = sources.iter().map(|s| s.id.to_string()).collect();
        let mut conflicts: Vec<String> = Vec::new();
        // Scheme → claimants ordered by publication; the winner is the
        // lexicographically greatest source id (no built-in plane exists for
        // schemes: internal families are reserved outright).
        let mut claims: BTreeMap<String, Vec<(String, SchemeBinding)>> = BTreeMap::new();

        let record_claim =
            |source: &RuntimeSource,
             conflicts: &mut Vec<String>,
             claims: &mut BTreeMap<String, Vec<(String, SchemeBinding)>>| {
                let label = source.id.to_string();
                let mut declared: BTreeSet<&str> = BTreeSet::new();
                for schema in &source.schemas {
                    let scheme = schema.scheme.as_str();
                    if SchemeRegistry::is_internal_scheme(scheme) {
                        conflicts.push(format!(
                            "source {label}: internal scheme `{scheme}` is reserved and cannot \
                         be registered"
                        ));
                        continue;
                    }
                    if !SchemeRegistry::is_valid_scheme_token(scheme) {
                        conflicts.push(format!(
                            "source {label}: scheme `{scheme}` must be a `[a-zA-Z0-9_-]` token \
                         of at least two characters without `__`"
                        ));
                        continue;
                    }
                    // Bundle sources keep their tools view-scoped (sidecar
                    // activation), so their schema claims name the owning
                    // tool by its `bundle:{id}/tool/{local}` stable id rather
                    // than a registry export.
                    if source.id.kind() != RuntimeSourceKind::Bundle
                        && source
                            .exports
                            .iter()
                            .all(|export| export.canonical_name != schema.canonical_tool)
                    {
                        conflicts.push(format!(
                            "source {label}: schema `{scheme}` claims tool `{}` which it does \
                         not export",
                            schema.canonical_tool
                        ));
                        continue;
                    }
                    if !declared.insert(scheme) {
                        conflicts.push(format!(
                            "source {label}: scheme `{scheme}` is declared more than once"
                        ));
                        continue;
                    }
                    claims.entry(scheme.to_string()).or_default().push((
                        label.clone(),
                        SchemeBinding::new(
                            label.clone(),
                            schema.canonical_tool.clone(),
                            schema.writable,
                        ),
                    ));
                }
            };

        for (id, source) in &self.sources {
            if replaced.contains(&id.to_string()) {
                continue;
            }
            record_claim(source, &mut conflicts, &mut claims);
        }
        for source in sources {
            record_claim(source, &mut conflicts, &mut claims);
        }

        // Adjudication: between two sources the lexicographically greater
        // source id string wins, mirroring the bare-name mask order.
        let schemes = claims
            .into_iter()
            .filter_map(|(scheme, mut claimants)| {
                claimants.sort_by(|left, right| left.0.cmp(&right.0));
                claimants.pop().map(|(_, binding)| (scheme, binding))
            })
            .collect();
        (conflicts, schemes)
    }

    /// Insert or replace MCP/plugin sources on this candidate.
    pub fn upsert_sources(
        &mut self,
        sources: Vec<RuntimeSource>,
    ) -> Result<(), RuntimeRefreshError> {
        let mut ids = BTreeSet::new();
        for source in &sources {
            if !ids.insert(source.id.clone()) {
                return Err(RuntimeRefreshError::InvalidCandidate(format!(
                    "duplicate runtime source {}",
                    source.id
                )));
            }
        }
        self.validate_candidate_source_names(&sources)
            .map(|masks| {
                self.masks = masks;
            })?;
        self.schemes = self.validate_candidate_source_schemes(&sources)?;

        for source in &sources {
            if let Some(previous) = self.sources.remove(&source.id) {
                for export in previous.exports {
                    self.tools.remove(&export.canonical_name);
                }
            }
        }
        for source in sources {
            let mut declared = BTreeSet::new();
            for export in &source.exports {
                if !declared.insert(export.declared_id.as_str()) {
                    return Err(RuntimeRefreshError::InvalidCandidate(format!(
                        "duplicate export {} for source {}",
                        export.declared_id, source.id
                    )));
                }
                if export.tool.name() != export.canonical_name {
                    return Err(RuntimeRefreshError::InvalidCandidate(format!(
                        "export {} canonical name {} does not match tool name {}",
                        export.declared_id,
                        export.canonical_name,
                        export.tool.name()
                    )));
                }
                // Contested aliases are never registered on the tool plane;
                // the bare name resolves through the snapshot mask table and
                // compiled views instead.
                let registered_aliases = export
                    .aliases
                    .iter()
                    .filter(|alias| !self.masks.contains_key(alias.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                let identity = runtime_source_dispatch_identity(&source, export)?;
                self.tools
                    .register_with_permission_and_aliases_and_dispatch_identity(
                        export.tool.clone(),
                        export.permission,
                        &registered_aliases,
                        identity,
                    )?;
            }
            let mut skill_ids = BTreeSet::new();
            for skill in &source.skills {
                if !skill_ids.insert(skill.stable_id.as_str()) {
                    return Err(RuntimeRefreshError::InvalidCandidate(format!(
                        "duplicate Skill {} for source {}",
                        skill.stable_id, source.id
                    )));
                }
                if skill.local_id.is_empty() || skill.entry.name != skill.local_id {
                    return Err(RuntimeRefreshError::InvalidCandidate(format!(
                        "Skill {} local id does not match parsed name for source {}",
                        skill.stable_id, source.id
                    )));
                }
                match source.id.kind {
                    RuntimeSourceKind::Bundle => {
                        let Some(bundle_path) = skill.stable_id.strip_prefix("bundle:") else {
                            return Err(RuntimeRefreshError::InvalidCandidate(format!(
                                "bundle source {} published non-bundle Skill {}",
                                source.id, skill.stable_id
                            )));
                        };
                        let expected =
                            format!("{}/skill/{}", source.id.configured_id, skill.local_id);
                        if bundle_path != expected {
                            return Err(RuntimeRefreshError::InvalidCandidate(format!(
                                "bundle Skill {} is not owned by source {}",
                                skill.stable_id, source.id
                            )));
                        }
                    }
                    RuntimeSourceKind::Mcp | RuntimeSourceKind::Plugin => {
                        if skill.stable_id.starts_with("bundle:") {
                            return Err(RuntimeRefreshError::InvalidCandidate(format!(
                                "non-bundle source {} cannot publish bundle Skill {}",
                                source.id, skill.stable_id
                            )));
                        }
                    }
                }
            }
            self.sources.insert(source.id.clone(), source);
        }
        Ok(())
    }

    /// Replace every source of one kind while preserving other source kinds.
    pub fn replace_sources_of_kind(
        &mut self,
        kind: RuntimeSourceKind,
        sources: Vec<RuntimeSource>,
    ) -> Result<(), RuntimeRefreshError> {
        if let Some(source) = sources.iter().find(|source| source.id.kind() != kind) {
            return Err(RuntimeRefreshError::InvalidCandidate(format!(
                "source {} does not belong to replacement kind {kind:?}",
                source.id
            )));
        }
        let removed = self
            .sources
            .keys()
            .filter(|id| id.kind() == kind)
            .cloned()
            .collect::<BTreeSet<_>>();
        self.remove_sources(&removed);
        self.upsert_sources(sources)
    }

    /// Remove sources by id from this candidate.
    ///
    /// The mask table is re-derived from the retained sources, so masks whose
    /// winner was removed dissolve. Note that a losing source's contested
    /// alias was never registered on the tool plane; it becomes resolvable
    /// again through the bare plane only after that source re-publishes, while
    /// its qualified canonical spelling always stays available.
    pub fn remove_sources(&mut self, removed: &BTreeSet<RuntimeSourceId>) {
        for id in removed {
            if let Some(source) = self.sources.remove(id) {
                for export in source.exports {
                    self.tools.remove(&export.canonical_name);
                }
            }
        }
        self.recompute_masks_after_removal();
        self.recompute_schemes_after_removal();
    }

    fn replace_skills(&mut self, workdir: &Path, skills: Vec<SkillCatalogEntry>) {
        let existing = self
            .skills
            .get(workdir)
            .map_or(&[][..], |current| current.as_slice());
        if existing == skills {
            return;
        }
        if skills.is_empty() {
            self.skills.remove(workdir);
        } else {
            self.skills.insert(workdir.to_path_buf(), Arc::new(skills));
        }
    }

    fn logically_matches(&self, snapshot: &RuntimeSnapshot) -> bool {
        self.catalog.bundles().bundles() == snapshot.catalog.bundles().bundles()
            && self.tools.logically_matches(&snapshot.tools)
            && self.skills == snapshot.skills
            && sources_match(&self.sources, &snapshot.sources)
    }
}

impl RuntimeSourceId {
    #[must_use]
    /// Build a source id from kind and configured identifier.
    pub fn new(kind: RuntimeSourceKind, configured_id: impl Into<String>) -> Self {
        Self {
            kind,
            configured_id: configured_id.into(),
        }
    }

    #[must_use]
    /// Construct a statically prepared bundle [`RuntimeSourceId`].
    pub fn bundle(configured_id: impl Into<String>) -> Self {
        Self::new(RuntimeSourceKind::Bundle, configured_id)
    }

    #[must_use]
    /// Construct an MCP [`RuntimeSourceId`].
    pub fn mcp(configured_id: impl Into<String>) -> Self {
        Self::new(RuntimeSourceKind::Mcp, configured_id)
    }

    #[must_use]
    /// Construct a plugin [`RuntimeSourceId`].
    pub fn plugin(configured_id: impl Into<String>) -> Self {
        Self::new(RuntimeSourceKind::Plugin, configured_id)
    }

    #[must_use]
    /// Return the source kind.
    pub fn kind(&self) -> RuntimeSourceKind {
        self.kind
    }

    #[must_use]
    /// Return the configured id string.
    pub fn configured_id(&self) -> &str {
        &self.configured_id
    }
}

impl std::fmt::Display for RuntimeSourceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            RuntimeSourceKind::Bundle => "bundle",
            RuntimeSourceKind::Mcp => "mcp",
            RuntimeSourceKind::Plugin => "plugin",
        };
        write!(formatter, "{kind}:{}", self.configured_id)
    }
}

impl RuntimeSourceExport {
    #[must_use]
    /// Build one export describing a tool and its aliases.
    pub fn tool(
        declared_id: impl Into<String>,
        canonical_name: impl Into<String>,
        aliases: Vec<String>,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
    ) -> Self {
        Self {
            declared_id: declared_id.into(),
            canonical_name: canonical_name.into(),
            aliases,
            tool,
            permission,
        }
    }
}
impl RuntimeSourceSkill {
    /// Build one source-owned Skill entry from a parsed contribution.
    #[must_use]
    pub fn new(
        stable_id: impl Into<String>,
        local_id: impl Into<String>,
        aliases: Vec<String>,
        digest: impl Into<String>,
        content: impl Into<String>,
        entry: SkillCatalogEntry,
    ) -> Self {
        Self {
            stable_id: stable_id.into(),
            local_id: local_id.into(),
            aliases,
            digest: digest.into(),
            content: content.into(),
            entry,
        }
    }

    /// Stable resource id used in bundle resource views.
    #[must_use]
    pub fn stable_id(&self) -> &str {
        &self.stable_id
    }

    /// Bundle or plugin-local Skill id.
    #[must_use]
    pub fn local_id(&self) -> &str {
        &self.local_id
    }

    /// Declared SHA-256 digest of the complete Skill content.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Complete Skill Markdown declaration bytes.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Parsed Skill metadata retained by the source.
    #[must_use]
    pub fn entry(&self) -> &SkillCatalogEntry {
        &self.entry
    }
}

impl RuntimeSource {
    /// Build a published source with exports, Skills, and an empty resource map.
    #[must_use]
    pub fn new(
        id: RuntimeSourceId,
        declaration_digest: [u8; 32],
        owner: Arc<dyn RuntimeSourceOwner>,
        exports: Vec<RuntimeSourceExport>,
    ) -> Self {
        Self {
            id,
            declaration_digest,
            owner,
            exports,
            skills: Vec::new(),
            resources: Arc::new(BTreeMap::new()),
            schemas: Vec::new(),
            hooks: None,
            apis: None,
            permission_modes: Vec::new(),
        }
    }

    /// The API endpoints attached with [`Self::with_apis`], if any.
    #[must_use]
    pub fn apis(&self) -> Option<&crate::bundle_apis::SourceApis> {
        self.apis.as_ref()
    }

    /// Attach the API endpoints this source's process serves.
    ///
    /// The provider is retained together with the source owner, so a request
    /// that resolved this generation keeps its process alive until it
    /// completes even if a newer generation is published meanwhile. An empty
    /// `apis` list attaches nothing.
    #[must_use]
    pub fn with_apis(
        mut self,
        mut apis: Vec<crate::bundle_apis::SourceApi>,
        provider: Arc<dyn crate::bundle_apis::BundleApiProvider>,
    ) -> Self {
        if apis.is_empty() {
            self.apis = None;
            return self;
        }
        apis.sort_by(|left, right| left.id.cmp(&right.id));
        self.apis = Some(crate::bundle_apis::SourceApis {
            apis,
            provider,
            _owner: Some(Arc::clone(&self.owner)),
        });
        self
    }

    /// Attach the session permission modes this source's process approves
    /// through its `permission.approve` hook (sorted by id).
    #[must_use]
    pub fn with_permission_modes(
        mut self,
        mut modes: Vec<crate::permission_mode::RuntimePermissionMode>,
    ) -> Self {
        modes.sort_by(|left, right| left.id.cmp(&right.id));
        self.permission_modes = modes;
        self
    }

    /// The session permission modes this source declares.
    #[must_use]
    pub fn permission_modes(&self) -> &[crate::permission_mode::RuntimePermissionMode] {
        &self.permission_modes
    }

    /// Attach process hooks retained by this immutable runtime generation.
    #[must_use]
    pub fn with_hooks(mut self, hooks: Arc<dyn crate::hooks::HookDispatcher>) -> Self {
        self.hooks = Some(Arc::new(crate::bundle_hooks::ScopedBundleHooks::retaining(
            hooks,
            Arc::clone(&self.owner),
        )));
        self
    }

    /// Attach parsed Skill contributions to the source.
    #[must_use]
    pub fn with_skills(mut self, skills: Vec<RuntimeSourceSkill>) -> Self {
        self.skills = skills;
        self
    }

    /// Attach external URI-scheme claims to the source.
    #[must_use]
    pub fn with_schemas(mut self, schemas: Vec<SourceSchema>) -> Self {
        self.schemas = schemas;
        self
    }

    #[must_use]
    /// Attach opaque JSON resources to the source.
    pub fn with_resources(mut self, resources: BTreeMap<String, Value>) -> Self {
        self.resources = Arc::new(resources);
        self
    }

    #[must_use]
    /// Borrow the source identifier.
    pub fn id(&self) -> &RuntimeSourceId {
        &self.id
    }

    #[must_use]
    /// The external URI-scheme claims this source makes.
    pub fn schemas(&self) -> &[SourceSchema] {
        &self.schemas
    }
}

fn sources_match(
    left: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    right: &BTreeMap<RuntimeSourceId, RuntimeSource>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(id, left)| {
            right.get(id).is_some_and(|right| {
                left.declaration_digest == right.declaration_digest
                    && Arc::ptr_eq(&left.owner, &right.owner)
                    && match (&left.hooks, &right.hooks) {
                        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                        (None, None) => true,
                        _ => false,
                    }
                    && match (&left.apis, &right.apis) {
                        (Some(left), Some(right)) => {
                            left.apis == right.apis && Arc::ptr_eq(&left.provider, &right.provider)
                        }
                        (None, None) => true,
                        _ => false,
                    }
                    && left.resources == right.resources
                    && left.permission_modes == right.permission_modes
                    && left.schemas == right.schemas
                    && left.skills.len() == right.skills.len()
                    && left.skills.iter().zip(&right.skills).all(|(left, right)| {
                        left.stable_id == right.stable_id
                            && left.local_id == right.local_id
                            && left.aliases == right.aliases
                            && left.digest == right.digest
                            && left.content == right.content
                            && left.entry == right.entry
                    })
                    && left.exports.len() == right.exports.len()
                    && left
                        .exports
                        .iter()
                        .zip(&right.exports)
                        .all(|(left, right)| {
                            left.declared_id == right.declared_id
                                && left.canonical_name == right.canonical_name
                                && left.aliases == right.aliases
                                && left.permission == right.permission
                                && Arc::ptr_eq(&left.tool, &right.tool)
                        })
            })
        })
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

impl TurnBinding {
    /// The declaring bundle's process hooks when `bundle_id` publishes the
    /// session permission mode `mode` in this generation; `None` when the
    /// bundle is gone, no longer declares the mode, or has no process.
    #[must_use]
    pub fn permission_mode_hooks(
        &self,
        bundle_id: &str,
        mode: &str,
    ) -> Option<Arc<dyn crate::hooks::HookDispatcher>> {
        let source = self
            .snapshot
            .sources
            .get(&RuntimeSourceId::bundle(bundle_id))?;
        if !source
            .permission_modes
            .iter()
            .any(|declared| declared.id == mode)
        {
            return None;
        }
        self.bundle_hooks(bundle_id)
    }

    /// Retain one explicitly selected bundle's process hooks from this generation.
    #[must_use]
    pub fn bundle_hooks(&self, bundle_id: &str) -> Option<Arc<dyn crate::hooks::HookDispatcher>> {
        if !self
            .snapshot
            .catalog
            .bundles()
            .bundles()
            .iter()
            .any(|bundle| bundle.identity().id == bundle_id)
        {
            return None;
        }
        self.snapshot
            .sources
            .get(&RuntimeSourceId::bundle(bundle_id))?
            .hooks
            .clone()
    }

    /// Read a bundle-owned Skill from this immutable generation.
    #[must_use]
    pub fn bundle_skill_content(&self, bundle_id: &str, local_id: &str) -> Option<&str> {
        self.snapshot
            .catalog
            .bundles()
            .bundles()
            .iter()
            .find(|bundle| bundle.identity().id == bundle_id)?
            .skills()
            .iter()
            .find(|skill| skill.local_id == local_id)
            .map(|skill| skill.content.as_str())
    }

    /// Hooks bound to one agent, in dispatch order.
    ///
    /// Every agent gets every installed Plugin-kind bundle's hooks and every
    /// hook-carrying Plugin source's hooks (a scope's project plugins), in
    /// stable ascending source-id order. A bundle agent (AgentBundle, AgentSetBundle,
    /// WorkflowBundle) additionally gets its own bundle's process hooks,
    /// filtered to the agent's `hook_refs`, after the Plugin hooks. An unknown
    /// agent gets none. Plugin entries are the retained source dispatchers
    /// themselves, so callers that merge chains can deduplicate by pointer.
    /// No live registry read occurs.
    #[must_use]
    pub fn bundle_hooks_for_agent(
        &self,
        stable_agent_id: &str,
    ) -> Vec<Arc<dyn crate::hooks::HookDispatcher>> {
        let Some(agent) = self.resolve_agent(stable_agent_id) else {
            return Vec::new();
        };
        let owner = agent.origin.bundle_id();
        let mut hooks = self.plugin_bundle_hooks(owner);
        if let Some(bundle_id) = owner
            && let Some(owner_hooks) = self.bundle_hooks(bundle_id)
            && let Ok(policy) = self.agent_resource_policy(stable_agent_id)
            && !policy.canonical_hook_ids.is_empty()
        {
            hooks.push(Arc::new(crate::bundle_hooks::ScopedBundleHooks::new(
                owner_hooks,
                &policy.canonical_hook_ids,
            )));
        }
        hooks
    }

    /// Every installed Plugin-kind bundle's hooks, then every Plugin-kind
    /// source's hooks (a scope's project plugins), in stable ascending
    /// source-id order, excluding bundle `exclude` so an owner is never
    /// dispatched twice.
    fn plugin_bundle_hooks(
        &self,
        exclude: Option<&str>,
    ) -> Vec<Arc<dyn crate::hooks::HookDispatcher>> {
        self.snapshot
            .sources
            .values()
            .filter(|source| {
                source.id.kind() == RuntimeSourceKind::Plugin
                    || (source.id.kind() == RuntimeSourceKind::Bundle
                        && exclude != Some(source.id.configured_id())
                        && self
                            .snapshot
                            .catalog
                            .bundles()
                            .bundles()
                            .iter()
                            .any(|bundle| {
                                bundle.identity().id == source.id.configured_id()
                                    && bundle.plugin_bundle().is_some()
                            }))
            })
            .filter_map(|source| source.hooks.clone())
            .collect()
    }

    /// Return a deterministic identity for the currently supported complete
    /// runtime view. Views with unidentifiable sources are intentionally
    /// unavailable until their semantic sections have a canonical encoding.
    #[must_use]
    /// Domain-separated fingerprint of agents, tools, sources, and permissions.
    ///
    /// v2 folds in the compiled-in built-in roster digest, which replaced the
    /// prepared-catalog digests the retired built-in bundles contributed.
    pub fn semantic_fingerprint_v1(&self, permission: &PermissionPlane) -> Option<[u8; 32]> {
        let catalog_identity = self.snapshot.catalog.semantic_identity_v1()?;
        let permission_identity = permission.semantic_identity_v1()?;
        let mut bytes = Vec::new();
        append_identity_bytes(&mut bytes, RUNTIME_SEMANTIC_FINGERPRINT_DOMAIN_V2).ok()?;

        append_identity_tag(&mut bytes, 1);
        append_identity_bytes(&mut bytes, &catalog_identity).ok()?;

        // The explicit empty section represents the none effective view.
        append_identity_tag(&mut bytes, 2);
        append_identity_count(&mut bytes, 0).ok()?;

        append_identity_tag(&mut bytes, 3);
        append_tool_view_identity(&mut bytes, &self.snapshot.basic_tools)?;

        append_identity_tag(&mut bytes, 4);
        append_tool_view_identity(&mut bytes, &self.snapshot.tools)?;

        append_identity_tag(&mut bytes, 5);
        let skills = self
            .snapshot
            .skills
            .get(&self.place.workdir)
            .map_or(&[][..], |skills| skills.as_slice());
        append_skill_view_identity(&mut bytes, skills)?;

        append_identity_tag(&mut bytes, 6);
        append_runtime_source_view_identity(
            &mut bytes,
            &self.snapshot.sources,
            &self.snapshot.tools,
        )?;

        append_identity_tag(&mut bytes, 7);
        append_identity_bytes(&mut bytes, &permission_identity).ok()?;

        if !self.agent_model_configuration.builtin.is_empty()
            || self
                .agent_model_configuration
                .bundles
                .values()
                .any(|models| !models.is_empty())
        {
            append_identity_tag(&mut bytes, 8);
            append_model_configuration_identity(&mut bytes, &self.agent_model_configuration)
                .ok()?;
        }
        if !self.session_agent_models.is_empty() {
            append_identity_tag(&mut bytes, 9);
            append_model_map_identity(&mut bytes, &self.session_agent_models).ok()?;
        }

        Some(Sha256::digest(bytes).into())
    }

    #[must_use]
    /// Config generation of the retained snapshot.
    pub fn generation(&self) -> ConfigGeneration {
        self.snapshot.generation
    }

    /// Look up a remembered model by exact stable Agent id in this binding.
    ///
    /// The lookup reads the immutable preference map captured when this binding
    /// was created. Later registry publications do not affect the returned
    /// reference or any other lookup through this binding.
    ///
    /// # Arguments
    ///
    /// * `stable_id` - Exact catalog stable id of the Agent to look up.
    ///
    /// # Returns
    ///
    /// The remembered model for `stable_id`, or `None` when no preference was
    /// captured for that Agent.
    #[must_use]
    pub fn agent_model_preference(&self, stable_id: &str) -> Option<&ModelRef> {
        self.agent_model_preferences.get(stable_id)
    }

    /// Replace the temporary Agent model map captured by this binding.
    ///
    /// The returned binding owns an immutable copy; later calls on the
    /// registry or other bindings cannot change this map. An empty map keeps
    /// the historical no-override behavior.
    #[must_use]
    pub fn with_session_agent_models(mut self, models: BTreeMap<String, ModelRef>) -> Self {
        self.session_agent_models = Arc::new(models);
        self
    }

    /// Replace the captured user-file model configuration on this binding.
    ///
    /// This is useful after a successful configuration-file write when the
    /// caller already has a fresh binding and must not trigger another runtime
    /// bind or skill discovery. The captured Session override map is retained.
    #[must_use]
    pub fn with_agent_model_configuration(
        mut self,
        configuration: AgentModelConfiguration,
    ) -> Self {
        self.agent_model_configuration = Arc::new(configuration);
        self
    }

    /// Look up the user-file model for an Agent's original catalog origin.
    ///
    /// Authored direct/category policy is intentionally not considered here;
    /// this reports only the matching global built-in or owning-bundle file
    /// entry captured when the binding was made.
    #[must_use]
    pub fn configured_agent_model(&self, stable_id: &str) -> Option<&ModelRef> {
        let definition = self.snapshot.catalog.resolve(stable_id)?;
        match definition.origin {
            AgentOrigin::Builtin => self
                .agent_model_configuration
                .builtin
                .get(definition.stable_id),
            AgentOrigin::Bundle { bundle_id } => self
                .agent_model_configuration
                .bundles
                .get(bundle_id)
                .and_then(|models| models.get(definition.stable_id)),
        }
    }

    /// Look up a temporary Session-tree model override for an Agent.
    #[must_use]
    pub fn session_agent_model(&self, stable_id: &str) -> Option<&ModelRef> {
        self.session_agent_models.get(stable_id)
    }

    #[must_use]
    /// Working directory this turn was bound to.
    pub fn workdir(&self) -> &Path {
        &self.place.workdir
    }

    /// Catalog scope this binding was bound in.
    #[must_use]
    pub fn scope(&self) -> &CatalogScope {
        &self.place.scope
    }

    /// Scope bundle id to source directory for this binding's scope (empty
    /// without a scope overlay).
    #[must_use]
    pub fn project_bundle_dirs(&self) -> &BTreeMap<String, PathBuf> {
        &self.place.project_bundle_dirs
    }

    #[must_use]
    /// Agent catalog (built-ins plus installed bundles) retained by this binding.
    pub fn agent_catalog(&self) -> &AgentCatalog {
        &self.snapshot.catalog
    }

    #[must_use]
    /// Installed-bundle catalog behind this binding's agent catalog.
    pub fn bundle_catalog(&self) -> &BundleCatalog {
        self.snapshot.catalog.bundles()
    }

    #[must_use]
    /// Published bare-name mask table: contested bare name → the canonical
    /// name of the winning tool. View compilation resolves every contested
    /// name to its winner; qualified canonical spellings always work.
    pub fn masks(&self) -> &BTreeMap<String, String> {
        &self.snapshot.masks
    }

    /// Every claimant of `bare` ordered by the masking total order: built-ins
    /// first, then contributed sources by ascending source id, with the active
    /// provider last. Each entry is `(owner source label, canonical name)`.
    /// Uncontested names yield zero or one claim.
    #[must_use]
    pub fn mask_chain(&self, bare: &str) -> Vec<(String, String)> {
        let rank = |label: &str| (u8::from(label != "built-in"), label.to_string());
        let mut claims: Vec<(String, String)> = Vec::new();
        for source in self.snapshot.sources.values() {
            let label = source.id.to_string();
            for export in &source.exports {
                let canonical_claim = export.canonical_name.as_str() == bare;
                let alias_claim = export.aliases.iter().any(|alias| alias == bare);
                if canonical_claim || alias_claim {
                    let entry = (label.clone(), export.canonical_name.clone());
                    if !claims.contains(&entry) {
                        claims.push(entry);
                    }
                }
            }
        }
        let source_canonical = claims.iter().any(|(_, canonical)| canonical == bare);
        if !source_canonical
            && self
                .snapshot
                .tools
                .canonical_tools()
                .iter()
                .any(|(name, _)| name == bare)
        {
            claims.push(("built-in".to_string(), bare.to_string()));
        }
        claims.sort_by_key(|(label, _)| rank(label));
        claims
    }

    #[must_use]
    /// Published scheme table: registered external URI scheme → the binding of
    /// the winning source. Mirrors [`TurnBinding::masks`]. View compilation
    /// exposes a scheme for dispatch only when this view also contains the
    /// binding's owning tool.
    pub fn schemes(&self) -> &BTreeMap<String, SchemeBinding> {
        &self.snapshot.schemes
    }

    /// Every claimant of `scheme` ordered by ascending source id, with the
    /// active provider (the lexicographically greatest source id) last. Each
    /// entry is `(owner source label, canonical tool name)`. Unclaimed schemes
    /// yield an empty chain.
    #[must_use]
    pub fn scheme_chain(&self, scheme: &str) -> Vec<(String, String)> {
        self.snapshot
            .sources
            .values()
            .filter_map(|source| {
                let claim = source.schemas.iter().find(|claim| claim.scheme == scheme)?;
                Some((source.id.to_string(), claim.canonical_tool.clone()))
            })
            .collect()
    }

    #[must_use]
    /// Look up an agent by stable id, whatever its origin.
    pub fn resolve_agent(&self, stable_id: &str) -> Option<AgentDefinition<'_>> {
        self.snapshot
            .catalog
            .resolve(stable_id)
            .map(|definition| self.overlay_agent_model(definition))
    }

    /// Resolve a user/model agent request against the catalog.
    pub fn resolve_requested_agent(
        &self,
        requested: Option<&str>,
    ) -> Result<AgentDefinition<'_>, BundleError> {
        let definition = self
            .snapshot
            .catalog
            .require(requested.unwrap_or("general"))?;
        Ok(self.overlay_agent_model(definition))
    }

    /// Resolve whether `caller` may spawn `target`.
    pub fn resolve_spawn(
        &self,
        caller: &str,
        requested: &str,
    ) -> Result<AgentDefinition<'_>, BundleError> {
        let definition = self.snapshot.catalog.resolve_spawn(caller, requested)?;
        Ok(self.overlay_agent_model(definition))
    }

    /// Agents the caller may spawn per can_spawn rules.
    pub fn spawnable_agents(&self, caller: &str) -> Result<Vec<AgentDefinition<'_>>, BundleError> {
        Ok(self
            .snapshot
            .catalog
            .spawnable(caller)?
            .into_iter()
            .map(|definition| self.overlay_agent_model(definition))
            .collect())
    }

    fn overlay_agent_model<'a>(&self, definition: AgentDefinition<'a>) -> AgentDefinition<'a> {
        let model = self
            .session_agent_model(definition.stable_id)
            .or_else(|| self.configured_agent_model(definition.stable_id));
        let Some(model) = model else {
            return definition;
        };
        let mut policy = definition.model_policy.into_owned();
        policy.model = Some(model.as_str().to_string());
        AgentDefinition {
            model_policy: std::borrow::Cow::Owned(policy),
            ..definition
        }
    }

    /// Compile the agent resource/tool policy for `stable_id`.
    pub fn agent_resource_policy(
        &self,
        stable_id: &str,
    ) -> Result<AgentResourcePolicy, BundleError> {
        let definition = self.snapshot.catalog.require(stable_id)?;
        let plane = AgentToolPlane::for_origin(&definition.origin);
        let bundle_id = definition.origin.bundle_id().map(str::to_string);
        // Built-ins own no bundle, so they carry no view and no hooks.
        let (resource_view, hook_refs) = match bundle_id.as_deref() {
            None => (ResourceView::default(), Vec::new()),
            Some(id) => {
                let (_, agent) = self
                    .snapshot
                    .catalog
                    .bundles()
                    .resolve_agent_entry(stable_id)
                    .ok_or_else(|| BundleError::UnknownAgentId {
                        agent_id: format!("{id}/{stable_id}"),
                    })?;
                (agent.resource_view.clone(), agent.hook_refs.clone())
            }
        };
        // Spawn rights are the effective (installed) spawn set: a built-in with
        // the ordinary scope, or a bundle `can_spawn` naming an installed agent.
        let spawn_rights = self
            .snapshot
            .catalog
            .spawnable(stable_id)
            .is_ok_and(|agents| !agents.is_empty());
        let mut policy = AgentResourcePolicy {
            bundle_id,
            plane,
            resource_view,
            spawn_rights,
            selected_bundle_tool_ids: Arc::new(Vec::new()),
            selected_bundle_skill_ids: Arc::new(Vec::new()),
            canonical_hook_ids: Arc::from(hook_refs),
        };
        if let Ok(partitions) = self.collect_resource_candidates(&policy)
            && let Ok(selected) = select_candidates_globally(&policy, &partitions)
        {
            policy.selected_bundle_tool_ids = Arc::new(
                selected
                    .tool
                    .iter()
                    .filter(|id| {
                        partitions
                            .tool
                            .get(*id)
                            .is_some_and(ResourceCandidate::is_bundle_local)
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            policy.selected_bundle_skill_ids = Arc::new(
                selected
                    .skill
                    .iter()
                    .filter(|id| {
                        partitions
                            .skill
                            .get(*id)
                            .is_some_and(ResourceCandidate::is_bundle_local)
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            );
        }
        Ok(policy)
    }

    /// Test-only: compile the policy for `stable_id` on an explicit plane.
    ///
    /// Production **always** derives the plane from the agent's origin through
    /// [`AgentToolPlane::for_origin`]. The resource-view compiler itself is
    /// plane-agnostic, so its unit tests pin a plane directly rather than
    /// resurrect an author-facing knob. Not reachable outside `cfg(test)`.
    #[cfg(test)]
    pub(crate) fn agent_resource_policy_on_plane(
        &self,
        stable_id: &str,
        plane: AgentToolPlane,
    ) -> Result<AgentResourcePolicy, BundleError> {
        let mut policy = self.agent_resource_policy(stable_id)?;
        policy.plane = plane;
        Ok(policy)
    }

    /// Report whether the selected agent's effective resource view needs a
    /// bundle sidecar in order to provide its executable capabilities.
    pub fn has_selected_bundle_sidecar_capability(
        &self,
        stable_id: &str,
    ) -> Result<bool, BundleError> {
        let policy = self.agent_resource_policy(stable_id)?;
        if policy.bundle_id.is_none() {
            // A built-in owns no bundle resources, so it never needs a sidecar.
            return Ok(false);
        }
        if policy
            .bundle_id
            .as_deref()
            .is_some_and(|id| self.bundle_hooks(id).is_some())
        {
            // Process extensions already own the complete declared executable surface.
            return Ok(false);
        }
        let partitions = self.collect_resource_candidates(&policy)?;
        let selected = select_candidates_globally(&policy, &partitions)?;
        let selected_bundle_tool = selected.tool.iter().any(|id| {
            partitions
                .tool
                .get(id)
                .is_some_and(ResourceCandidate::is_bundle_local)
        });
        Ok(selected_bundle_tool || !policy.canonical_hook_ids.is_empty())
    }

    fn collect_resource_candidates(
        &self,
        policy: &AgentResourcePolicy,
    ) -> Result<CandidatePartitions, BundleError> {
        let bundles = self.snapshot.catalog.bundles();
        let mut tool_candidates = BTreeMap::new();
        let mut skill_candidates = BTreeMap::new();
        let mut mcp_candidates = BTreeMap::new();

        // Bundle-local resources exist only for a bundle agent, and only from
        // its OWN bundle. Resolving a `bundle:` reference against the whole
        // catalog would let one bundle borrow another bundle's tools.
        if let Some(bundle_id) = policy.bundle_id.as_deref() {
            collect_bundle_tool_candidates(bundles, bundle_id, &mut tool_candidates)?;
            for reference in &policy.resource_view.allow {
                if !reference.starts_with("bundle:")
                    || kind_from_qualified_reference(reference) != Some("tool")
                    || tool_candidates.contains_key(reference)
                {
                    continue;
                }
                let (owner, resource) =
                    bundles.resolve_resource_entry(bundle_id, ExportKind::Tool, reference)?;
                if owner != bundle_id {
                    return Err(BundleError::ResourceNotInPlane {
                        bundle_id: bundle_id.to_string(),
                        reference: reference.clone(),
                        plane: policy.plane.as_str().to_string(),
                    });
                }
                tool_candidates.insert(
                    resource.stable_id.clone(),
                    ResourceCandidate::BundleLocal {
                        short_name: resource.local_id.clone(),
                        qualified_name: resource.stable_id.clone(),
                        aliases: resource.aliases.clone(),
                    },
                );
            }
            collect_bundle_skill_candidates(
                bundles,
                &self.snapshot.sources,
                bundle_id,
                &mut skill_candidates,
            )?;
            collect_bundle_mcp_candidates(bundles, bundle_id, &mut mcp_candidates)?;
        }

        collect_harness_tool_candidates(
            policy.plane,
            &self.snapshot.basic_tools,
            &self.snapshot.tools,
            &self.snapshot.sources,
            &self.snapshot.masks,
            bundles,
            &mut tool_candidates,
        );
        collect_harness_skill_candidates(
            policy.plane,
            self.skills(),
            &self.snapshot.sources,
            bundles,
            &mut skill_candidates,
        );
        collect_harness_mcp_candidates(
            policy.plane,
            &self.snapshot.sources,
            &self.snapshot.masks,
            bundles,
            &mut mcp_candidates,
        );

        Ok(CandidatePartitions {
            tool: tool_candidates,
            skill: skill_candidates,
            mcp: mcp_candidates,
        })
    }

    pub(crate) fn compile_agent_resources(
        &self,
        policy: &AgentResourcePolicy,
    ) -> Result<Arc<CompiledResourceView>, BundleError> {
        self.compile_agent_resources_with_sidecar_tools(policy, &[])
    }

    pub(crate) fn compile_agent_resources_with_sidecar_tools(
        &self,
        policy: &AgentResourcePolicy,
        sidecar_tools: &[ResolvedTool],
    ) -> Result<Arc<CompiledResourceView>, BundleError> {
        let view = &policy.resource_view;
        let bundle_id = policy.diagnostic_scope();
        let namespace = policy.namespace();

        let mut sidecar_tools_by_name = BTreeMap::new();
        for resolved in sidecar_tools {
            let canonical_id = resolved.tool.name().to_string();
            if sidecar_tools_by_name
                .insert(canonical_id.clone(), resolved.clone())
                .is_some()
            {
                return Err(BundleError::NamespaceCollision {
                    bundle_id: bundle_id.to_string(),
                    name: canonical_id,
                });
            }
        }

        let partitions = self.collect_resource_candidates(policy)?;
        let selected = select_candidates_globally(policy, &partitions)?;

        let view_aliases = resolve_view_aliases(policy, &partitions, &selected, view)?;
        let tool_view_aliases = aliases_for_kind("tool", &view_aliases);
        let skill_view_aliases = aliases_for_kind("skill", &view_aliases);
        let mcp_view_aliases = aliases_for_kind("mcp", &view_aliases);

        let mut tool_public = assign_public_names(
            bundle_id,
            namespace,
            "tool",
            &partitions.tool,
            &selected.tool,
            &tool_view_aliases,
            Some(&partitions.mcp),
        )?;
        let mut skill_public = assign_public_names(
            bundle_id,
            namespace,
            "skill",
            &partitions.skill,
            &selected.skill,
            &skill_view_aliases,
            None,
        )?;
        let mut mcp_public = assign_public_names(
            bundle_id,
            namespace,
            "mcp",
            &partitions.mcp,
            &selected.mcp,
            &mcp_view_aliases,
            Some(&partitions.tool),
        )?;

        inject_effective_aliases(
            bundle_id,
            "tool",
            &partitions.tool,
            &selected.tool,
            &tool_view_aliases,
            &mut tool_public,
        )?;
        inject_effective_aliases(
            bundle_id,
            "skill",
            &partitions.skill,
            &selected.skill,
            &skill_view_aliases,
            &mut skill_public,
        )?;
        inject_effective_aliases(
            bundle_id,
            "mcp",
            &partitions.mcp,
            &selected.mcp,
            &mcp_view_aliases,
            &mut mcp_public,
        )?;

        // Tool and MCP share the provider tool/schema/dispatch namespace.
        for name in tool_public.keys() {
            if mcp_public.contains_key(name) {
                return Err(BundleError::NamespaceCollision {
                    bundle_id: bundle_id.to_string(),
                    name: name.clone(),
                });
            }
        }

        let model_tool_names = model_schema_public_names(
            bundle_id,
            "tool",
            &partitions.tool,
            &tool_public,
            &tool_view_aliases,
        )?;
        let mut model_mcp_names = model_schema_public_names(
            bundle_id,
            "mcp",
            &partitions.mcp,
            &mcp_public,
            &mcp_view_aliases,
        )?;

        let skill_facade_selected = selected.tool.contains("harness:tool/skill");
        let has_harness_skill = selected.skill.iter().any(|id| {
            matches!(
                partitions.skill.get(id),
                Some(ResourceCandidate::HarnessSkill { .. })
            )
        });
        if has_harness_skill && !skill_facade_selected {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: "selected harness skills require the skill tool facade".to_string(),
            });
        }

        let mut tools = BTreeMap::new();
        let mut mcp_expansions = BTreeMap::new();
        for (public_name, canonical_id) in tool_public.iter().chain(mcp_public.iter()) {
            let (kind, candidates) = if partitions.tool.contains_key(canonical_id) {
                ("tool", &partitions.tool)
            } else {
                ("mcp", &partitions.mcp)
            };
            let candidate = candidates.get(canonical_id).ok_or_else(|| {
                BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: canonical_id.clone(),
                }
            })?;
            match candidate {
                ResourceCandidate::BundleLocal { .. } if kind == "tool" => {
                    let process_tool = canonical_id
                        .strip_prefix("bundle:")
                        .and_then(|id| id.split_once("/tool/"))
                        .and_then(|(owner, local)| {
                            self.snapshot
                                .sources
                                .get(&RuntimeSourceId::bundle(owner))
                                .and_then(|source| {
                                    source.exports.iter().find(|export| {
                                        export.declared_id == local
                                            && export.permission != ToolPermission::Mcp
                                    })
                                })
                        })
                        .map(|export| ResolvedTool {
                            tool: Arc::new(NamedTool::new(
                                canonical_id.clone(),
                                export.tool.clone(),
                            )),
                            permission: export.permission,
                        });
                    if let Some(resolved) = sidecar_tools_by_name
                        .get(canonical_id)
                        .cloned()
                        .or(process_tool)
                    {
                        tools.insert(public_name.clone(), resolved);
                    } else {
                        return Err(BundleError::UnsupportedBundleFeature {
                            bundle_id: bundle_id.to_string(),
                            feature: "resources.tools".to_string(),
                        });
                    }
                }
                ResourceCandidate::BundleLocal { .. } => {
                    let (owner, server_id) = canonical_id
                        .strip_prefix("bundle:")
                        .and_then(|id| id.split_once("/mcp/"))
                        .ok_or_else(|| BundleError::UnknownResourceReference {
                            bundle_id: bundle_id.to_string(),
                            kind: "mcp".into(),
                            reference: canonical_id.clone(),
                        })?;
                    let source = self
                        .snapshot
                        .sources
                        .get(&RuntimeSourceId::bundle(owner))
                        .ok_or_else(|| BundleError::UnsupportedBundleFeature {
                            bundle_id: bundle_id.to_string(),
                            feature: "resources.mcp".into(),
                        })?;
                    let prefix = format!("mcp/{server_id}/");
                    let mut expanded_names = BTreeSet::new();
                    for export in source
                        .exports
                        .iter()
                        .filter(|export| export.permission == ToolPermission::Mcp)
                    {
                        let Some(local) = export.declared_id.strip_prefix(&prefix) else {
                            continue;
                        };
                        let name = format!("{public_name}__{local}");
                        if tools.contains_key(&name) {
                            return Err(BundleError::NamespaceCollision {
                                bundle_id: bundle_id.into(),
                                name,
                            });
                        }
                        tools.insert(
                            name.clone(),
                            ResolvedTool {
                                tool: export.tool.clone(),
                                permission: export.permission,
                            },
                        );
                        expanded_names.insert(name);
                    }
                    mcp_expansions.insert(public_name.clone(), expanded_names);
                }
                ResourceCandidate::HarnessTool { resolved, .. }
                | ResourceCandidate::HarnessMcp { resolved, .. } => {
                    tools.insert(public_name.clone(), resolved.clone());
                }
                ResourceCandidate::BundleLocalSkill { .. }
                | ResourceCandidate::HarnessSkill { .. } => {
                    return Err(BundleError::UnknownResourceReference {
                        bundle_id: bundle_id.to_string(),
                        kind: kind.to_string(),
                        reference: canonical_id.clone(),
                    });
                }
            }
        }

        let mut skills = Vec::new();
        let mut ordered_skill_names = skill_public.keys().cloned().collect::<Vec<_>>();
        ordered_skill_names.sort();
        for public_name in ordered_skill_names {
            let canonical_id = skill_public.get(&public_name).ok_or_else(|| {
                BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: "skill".to_string(),
                    reference: public_name.clone(),
                }
            })?;
            let candidate = partitions.skill.get(canonical_id).ok_or_else(|| {
                BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: "skill".to_string(),
                    reference: canonical_id.clone(),
                }
            })?;
            let mut entry = match candidate {
                ResourceCandidate::BundleLocalSkill { entry, .. }
                | ResourceCandidate::HarnessSkill { entry, .. } => entry.clone(),
                ResourceCandidate::BundleLocal { .. }
                | ResourceCandidate::HarnessTool { .. }
                | ResourceCandidate::HarnessMcp { .. } => {
                    return Err(BundleError::UnknownResourceReference {
                        bundle_id: bundle_id.to_string(),
                        kind: "skill".to_string(),
                        reference: canonical_id.clone(),
                    });
                }
            };
            entry.name = public_name;
            skills.push(entry);
        }

        for (server_name, expanded) in mcp_expansions {
            if model_mcp_names.remove(&server_name) {
                for name in expanded {
                    if !is_model_tool_name(&name) {
                        return Err(BundleError::InvalidManifest {
                            source_name: bundle_id.into(),
                            detail: format!(
                                "expanded MCP tool `{name}` needs a shorter provider-safe server alias"
                            ),
                        });
                    }
                    model_mcp_names.insert(name);
                }
            }
        }
        let schemas = model_tool_names
            .iter()
            .chain(model_mcp_names.iter())
            .map(|public_name| {
                let resolved = tools.get(public_name).ok_or_else(|| {
                    BundleError::UnknownResourceReference {
                        bundle_id: bundle_id.to_string(),
                        kind: "tool".to_string(),
                        reference: public_name.clone(),
                    }
                })?;
                let mut schema = resolved.tool.schema();
                schema.name = ToolName::new(public_name.clone());
                Ok(schema)
            })
            .collect::<Result<Vec<_>, BundleError>>()?;

        let mut tools = self.with_scheme_dispatch(tools);
        let mut schemas = schemas;
        // Coordination: with the channel family loaded, every agent can read
        // its mail. A view without its own `read` gets a mail-only `read`
        // (never file access); injected after scheme dispatch so no external
        // scheme can reach through it.
        if partitions.tool.contains_key(&harness_id(
            "tool",
            crate::coordination::CHANNEL_MARKER_TOOL,
        )) && !selected.tool.contains(&harness_id("tool", "read"))
            && !tools.contains_key("read")
            && let Some(ResourceCandidate::HarnessTool { resolved, .. }) =
                partitions.tool.get(&harness_id("tool", "read"))
        {
            let channel_read: Arc<dyn Tool> = Arc::new(crate::coordination::ChannelReadTool::new(
                Arc::clone(&resolved.tool),
            ));
            schemas.push(channel_read.schema());
            tools.insert(
                "read".to_string(),
                ResolvedTool {
                    tool: channel_read,
                    permission: resolved.permission,
                },
            );
        }

        let compiled = Arc::new(CompiledResourceView {
            tools,
            schemas,
            skills: Arc::new(skills),
            canonical_hook_ids: Arc::clone(&policy.canonical_hook_ids),
            skill_facade_selected,
        });
        debug_assert_eq!(compiled.canonical_hook_ids(), policy.canonical_hook_ids());
        Ok(compiled)
    }

    /// Install the view's external-scheme dispatch on its read and write tools.
    ///
    /// Only schemes whose owning tool this view actually resolves become
    /// dispatchable — the binding table is intersected with the view's own tool
    /// set, and there is deliberately no registry-wide fallback. A view that
    /// resolves no schemes keeps its tools byte-for-byte unwrapped.
    fn with_scheme_dispatch(
        &self,
        mut tools: BTreeMap<String, ResolvedTool>,
    ) -> BTreeMap<String, ResolvedTool> {
        if self.snapshot.schemes.is_empty() {
            return tools;
        }
        let mut handlers = BTreeMap::new();
        for (scheme, binding) in self.snapshot.schemes.iter() {
            if let Some(resolved) = tools
                .values()
                .find(|resolved| resolved.tool.name() == binding.canonical_tool())
            {
                handlers.insert(
                    scheme.clone(),
                    SchemeHandler::new(binding.clone(), Arc::clone(&resolved.tool)),
                );
            }
        }
        if handlers.is_empty() {
            return tools;
        }
        let dispatch = SchemeDispatch::new(handlers);
        let wrapped = tools
            .into_iter()
            .map(|(public_name, resolved)| {
                let tool: Arc<dyn Tool> = match resolved.tool.name() {
                    "read" => Arc::new(SchemeReadTool::new(
                        Arc::clone(&resolved.tool),
                        dispatch.clone(),
                    )),
                    "write" => Arc::new(SchemeWriteTool::new(
                        Arc::clone(&resolved.tool),
                        dispatch.clone(),
                    )),
                    _ => resolved.tool,
                };
                (
                    public_name,
                    ResolvedTool {
                        tool,
                        permission: resolved.permission,
                    },
                )
            })
            .collect();
        tools = wrapped;
        tools
    }

    #[must_use]
    /// Model-facing tool schemas from this snapshot or view.
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.snapshot.tools.schemas()
    }

    #[must_use]
    /// Resolve a tool name or alias in this compiled view.
    pub fn resolve_tool(&self, name: &str) -> Option<ResolvedTool> {
        self.snapshot.tools.resolve(name)
    }

    #[must_use]
    /// Skill entries visible to this resource view.
    pub fn skills(&self) -> &[SkillCatalogEntry] {
        self.snapshot
            .skills
            .get(&self.place.workdir)
            .map_or(&[], |skills| skills.as_slice())
    }

    #[must_use]
    /// Build a skill plane over this view's skill snapshot.
    pub fn skill_plane(&self) -> SkillPlane {
        let skills = self
            .snapshot
            .skills
            .get(&self.place.workdir)
            .cloned()
            .unwrap_or_default();
        SkillPlane::from_snapshot(skills)
    }
}

impl CompiledResourceView {
    pub(crate) fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.schemas.clone()
    }

    pub(crate) fn resolve_tool(&self, name: &str) -> Option<ResolvedTool> {
        self.tools.get(name).cloned()
    }

    #[cfg(test)]
    pub(crate) fn public_tool_names(&self) -> BTreeSet<String> {
        self.tools.keys().cloned().collect()
    }

    pub(crate) fn skills(&self) -> &[SkillCatalogEntry] {
        self.skills.as_slice()
    }

    pub(crate) fn canonical_hook_ids(&self) -> &[String] {
        self.canonical_hook_ids.as_ref()
    }

    /// Prompt skill exposure for the bound agent. When the selected view
    /// includes the harness skill facade (even if aliased), only the on-demand
    /// index is appended; otherwise selected bundle-local static skill bodies
    /// are inlined once with their dispatchable spelling list.
    pub(crate) fn skills_prompt_section(&self) -> Option<String> {
        let skills = self.skills();
        if skills.is_empty() {
            return None;
        }
        if self.skill_facade_selected {
            return skill_index_with_spellings(skills);
        }
        Some(inline_selected_skills_with_spellings(skills))
    }

    pub(crate) fn skill_plane(&self) -> SkillPlane {
        SkillPlane::from_snapshot(self.skills.clone())
    }
}

#[derive(Clone)]
enum ResourceCandidate {
    BundleLocal {
        short_name: String,
        qualified_name: String,
        aliases: Vec<String>,
    },
    BundleLocalSkill {
        short_name: String,
        qualified_name: String,
        entry: SkillCatalogEntry,
        aliases: Vec<String>,
    },
    HarnessTool {
        short_name: String,
        qualified_name: String,
        resolved: ResolvedTool,
        aliases: Vec<String>,
    },
    HarnessSkill {
        short_name: String,
        qualified_name: String,
        entry: SkillCatalogEntry,
        aliases: Vec<String>,
    },
    HarnessMcp {
        short_name: String,
        qualified_name: String,
        resolved: ResolvedTool,
        aliases: Vec<String>,
    },
}

impl ResourceCandidate {
    fn short_name(&self) -> &str {
        match self {
            Self::BundleLocal { short_name, .. }
            | Self::BundleLocalSkill { short_name, .. }
            | Self::HarnessTool { short_name, .. }
            | Self::HarnessSkill { short_name, .. }
            | Self::HarnessMcp { short_name, .. } => short_name,
        }
    }

    fn qualified_name(&self) -> &str {
        match self {
            Self::BundleLocal { qualified_name, .. }
            | Self::BundleLocalSkill { qualified_name, .. }
            | Self::HarnessTool { qualified_name, .. }
            | Self::HarnessSkill { qualified_name, .. }
            | Self::HarnessMcp { qualified_name, .. } => qualified_name,
        }
    }

    fn is_bundle_local(&self) -> bool {
        matches!(
            self,
            Self::BundleLocal { .. } | Self::BundleLocalSkill { .. }
        )
    }

    fn aliases(&self) -> &[String] {
        match self {
            Self::BundleLocal { aliases, .. }
            | Self::BundleLocalSkill { aliases, .. }
            | Self::HarnessTool { aliases, .. }
            | Self::HarnessSkill { aliases, .. }
            | Self::HarnessMcp { aliases, .. } => aliases,
        }
    }
}

struct CandidatePartitions {
    tool: BTreeMap<String, ResourceCandidate>,
    skill: BTreeMap<String, ResourceCandidate>,
    mcp: BTreeMap<String, ResourceCandidate>,
}

struct SelectedIds {
    tool: BTreeSet<String>,
    skill: BTreeSet<String>,
    mcp: BTreeSet<String>,
}

fn group_skill_spellings(skills: &[SkillCatalogEntry]) -> Vec<(SkillCatalogEntry, Vec<String>)> {
    let mut groups: BTreeMap<String, (SkillCatalogEntry, BTreeSet<String>)> = BTreeMap::new();
    for skill in skills {
        let key = skill.path.to_string_lossy().into_owned();
        let entry = groups
            .entry(key)
            .or_insert_with(|| (skill.clone(), BTreeSet::new()));
        entry.1.insert(skill.name.clone());
    }
    let mut out = groups
        .into_values()
        .map(|(entry, names)| {
            let spellings = ordered_skill_spellings(names);
            let mut entry = entry;
            if let Some(primary) = spellings.first() {
                entry.name = primary.clone();
            }
            (entry, spellings)
        })
        .collect::<Vec<_>>();
    out.sort_by(|left, right| left.0.name.cmp(&right.0.name));
    out
}

fn ordered_skill_spellings(names: BTreeSet<String>) -> Vec<String> {
    let mut shorts = names
        .iter()
        .filter(|name| !name.contains(':'))
        .cloned()
        .collect::<Vec<_>>();
    shorts.sort();
    let mut qualified = names
        .into_iter()
        .filter(|name| name.contains(':'))
        .collect::<Vec<_>>();
    qualified.sort();
    shorts.extend(qualified);
    shorts
}

fn format_spelling_list(spellings: &[String]) -> String {
    match spellings {
        [] => String::new(),
        [only] => only.clone(),
        [primary, rest @ ..] => format!("{primary} (also: {})", rest.join(", ")),
    }
}

fn skill_index_with_spellings(skills: &[SkillCatalogEntry]) -> Option<String> {
    let groups = group_skill_spellings(skills);
    if groups.is_empty() {
        return None;
    }
    let mut section =
        "These skills are available on demand; read the named SKILL.md when relevant:".to_string();
    for (entry, spellings) in groups {
        section.push_str("\n- ");
        section.push_str(&format_spelling_list(&spellings));
        section.push_str(": ");
        section.push_str(&entry.description);
    }
    Some(section)
}

fn inline_selected_skills_with_spellings(skills: &[SkillCatalogEntry]) -> String {
    let mut section =
        "The following skills are embedded in full because no skill tool is available:".to_string();
    for (entry, spellings) in group_skill_spellings(skills) {
        section.push_str("\n\n## ");
        section.push_str(&format_spelling_list(&spellings));
        section.push('\n');
        section.push_str(&entry.description);
        section.push_str("\n\n");
        section.push_str(&entry.content);
    }
    section
}

fn harness_id(kind: &str, short: &str) -> String {
    format!("harness:{kind}/{short}")
}

fn namespace_qualified(namespace: &str, kind: &str, local_id: &str) -> String {
    format!("bundle:{namespace}/{kind}/{local_id}")
}

fn runtime_source_dispatch_identity(
    source: &RuntimeSource,
    export: &RuntimeSourceExport,
) -> Result<[u8; 32], RuntimeRefreshError> {
    let mut bytes = Vec::new();
    append_identity_bytes(&mut bytes, RUNTIME_SOURCE_DISPATCH_IDENTITY_DOMAIN_V1)?;
    append_identity_tag(&mut bytes, 1);
    append_identity_tag(
        &mut bytes,
        match source.id.kind {
            RuntimeSourceKind::Mcp => 0,
            RuntimeSourceKind::Plugin => 1,
            RuntimeSourceKind::Bundle => 2,
        },
    );
    append_identity_tag(&mut bytes, 2);
    append_identity_bytes(&mut bytes, source.id.configured_id.as_bytes())?;
    append_identity_tag(&mut bytes, 3);
    append_identity_bytes(&mut bytes, &source.declaration_digest)?;
    append_identity_tag(&mut bytes, 4);
    append_identity_count(&mut bytes, source.resources.len())?;
    for (key, value) in source.resources.iter() {
        append_identity_bytes(&mut bytes, key.as_bytes())?;
        append_canonical_json_value(&mut bytes, value)?;
    }
    append_identity_tag(&mut bytes, 5);
    append_identity_bytes(&mut bytes, export.declared_id.as_bytes())?;
    append_identity_tag(&mut bytes, 6);
    append_identity_bytes(&mut bytes, export.canonical_name.as_bytes())?;
    Ok(Sha256::digest(bytes).into())
}

fn append_identity_tag(bytes: &mut Vec<u8>, tag: u8) {
    bytes.push(tag);
}

fn append_identity_count(bytes: &mut Vec<u8>, count: usize) -> Result<(), RuntimeRefreshError> {
    let count = u64::try_from(count).map_err(|_| {
        RuntimeRefreshError::InvalidCandidate(
            "runtime source dispatch identity count exceeds u64".to_string(),
        )
    })?;
    bytes.extend_from_slice(&count.to_be_bytes());
    Ok(())
}

fn append_model_map_identity(
    bytes: &mut Vec<u8>,
    models: &BTreeMap<String, ModelRef>,
) -> Result<(), RuntimeRefreshError> {
    append_identity_count(bytes, models.len())?;
    for (agent_id, model) in models {
        append_identity_bytes(bytes, agent_id.as_bytes())?;
        append_identity_bytes(bytes, model.as_str().as_bytes())?;
    }
    Ok(())
}

fn append_model_configuration_identity(
    bytes: &mut Vec<u8>,
    configuration: &AgentModelConfiguration,
) -> Result<(), RuntimeRefreshError> {
    append_identity_tag(bytes, 1);
    append_model_map_identity(bytes, &configuration.builtin)?;
    append_identity_tag(bytes, 2);
    append_identity_count(bytes, configuration.bundles.len())?;
    for (bundle_id, models) in &configuration.bundles {
        append_identity_bytes(bytes, bundle_id.as_bytes())?;
        append_model_map_identity(bytes, models)?;
    }
    Ok(())
}

fn append_identity_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), RuntimeRefreshError> {
    let length = u64::try_from(value.len()).map_err(|_| {
        RuntimeRefreshError::InvalidCandidate(
            "runtime source dispatch identity length exceeds u64".to_string(),
        )
    })?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value);
    Ok(())
}

fn append_canonical_json_value(
    bytes: &mut Vec<u8>,
    value: &Value,
) -> Result<(), RuntimeRefreshError> {
    match value {
        Value::Null => append_identity_tag(bytes, 0),
        Value::Bool(value) => {
            append_identity_tag(bytes, 1);
            append_identity_tag(bytes, u8::from(*value));
        }
        Value::Number(value) => {
            append_identity_tag(bytes, 2);
            let value = value.to_string();
            append_identity_bytes(bytes, value.as_bytes())?;
        }
        Value::String(value) => {
            append_identity_tag(bytes, 3);
            append_identity_bytes(bytes, value.as_bytes())?;
        }
        Value::Array(values) => {
            append_identity_tag(bytes, 4);
            append_identity_count(bytes, values.len())?;
            for value in values {
                append_canonical_json_value(bytes, value)?;
            }
        }
        Value::Object(values) => {
            append_identity_tag(bytes, 5);
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            append_identity_count(bytes, entries.len())?;
            for (key, value) in entries {
                append_identity_bytes(bytes, key.as_bytes())?;
                append_canonical_json_value(bytes, value)?;
            }
        }
    }
    Ok(())
}

fn append_tool_view_identity(bytes: &mut Vec<u8>, tools: &ToolRegistrySnapshot) -> Option<()> {
    let mut canonical_tools = tools.canonical_tools();
    canonical_tools.sort_by(|left, right| left.0.cmp(&right.0));
    append_identity_count(bytes, canonical_tools.len()).ok()?;
    for (canonical_name, resolved) in canonical_tools {
        append_identity_tag(bytes, 1);
        append_identity_bytes(bytes, canonical_name.as_bytes()).ok()?;
        append_identity_tag(bytes, 2);
        append_tool_permission_identity(bytes, resolved.permission);

        let schema = resolved.tool.schema();
        append_identity_tag(bytes, 3);
        append_identity_bytes(bytes, schema.name.as_str().as_bytes()).ok()?;
        append_identity_tag(bytes, 4);
        append_identity_bytes(bytes, schema.description.as_bytes()).ok()?;
        append_identity_tag(bytes, 5);
        append_canonical_json_value(bytes, &schema.input_schema).ok()?;
        append_identity_tag(bytes, 6);
        match schema.output_schema {
            None => append_identity_tag(bytes, 0),
            Some(output_schema) => {
                append_identity_tag(bytes, 1);
                append_canonical_json_value(bytes, &output_schema).ok()?;
            }
        }

        append_identity_tag(bytes, 7);
        let dispatch_identity = tools.dispatch_identity_v1(&canonical_name)?;
        append_identity_bytes(bytes, &dispatch_identity).ok()?;

        append_identity_tag(bytes, 8);
        let mut aliases = tools.aliases_for_canonical(&canonical_name);
        aliases.sort();
        append_identity_count(bytes, aliases.len()).ok()?;
        for alias in aliases {
            append_identity_bytes(bytes, alias.as_bytes()).ok()?;
        }
    }
    Some(())
}

fn append_tool_permission_identity(bytes: &mut Vec<u8>, permission: ToolPermission) {
    append_identity_tag(
        bytes,
        match permission {
            ToolPermission::ReadOnly => 0,
            ToolPermission::Task => 1,
            ToolPermission::Tool => 2,
            ToolPermission::Command => 3,
            ToolPermission::Mcp => 4,
        },
    );
}

fn append_skill_view_identity(bytes: &mut Vec<u8>, skills: &[SkillCatalogEntry]) -> Option<()> {
    append_identity_count(bytes, skills.len()).ok()?;
    for skill in skills {
        append_identity_tag(bytes, 1);
        append_identity_bytes(bytes, skill.name.as_bytes()).ok()?;
        append_identity_tag(bytes, 2);
        append_identity_bytes(bytes, skill.description.as_bytes()).ok()?;
        append_identity_tag(bytes, 3);
        let content_digest = Sha256::digest(skill.content.as_bytes());
        append_identity_bytes(bytes, &content_digest).ok()?;
        append_identity_tag(bytes, 4);
        let mut allowed_tools = skill.allowed_tools.clone();
        allowed_tools.sort();
        append_identity_count(bytes, allowed_tools.len()).ok()?;
        for allowed_tool in allowed_tools {
            append_identity_bytes(bytes, allowed_tool.as_bytes()).ok()?;
        }
        append_identity_tag(bytes, 5);
        match skill.model.as_deref() {
            None => append_identity_tag(bytes, 0),
            Some(model) => {
                append_identity_tag(bytes, 1);
                append_identity_bytes(bytes, model.as_bytes()).ok()?;
            }
        }
        append_identity_tag(bytes, 6);
        append_identity_bytes(bytes, skill.path.to_str()?.as_bytes()).ok()?;
        append_identity_tag(bytes, 7);
        append_identity_bytes(bytes, skill.dir.to_str()?.as_bytes()).ok()?;
    }
    Some(())
}

fn append_runtime_source_view_identity(
    bytes: &mut Vec<u8>,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    tools: &ToolRegistrySnapshot,
) -> Option<()> {
    append_identity_count(bytes, sources.len()).ok()?;
    for (source_id, source) in sources {
        append_identity_tag(bytes, 1);
        append_identity_tag(
            bytes,
            match source_id.kind {
                RuntimeSourceKind::Mcp => 0,
                RuntimeSourceKind::Plugin => 1,
                RuntimeSourceKind::Bundle => 2,
            },
        );
        append_identity_tag(bytes, 2);
        append_identity_bytes(bytes, source_id.configured_id.as_bytes()).ok()?;
        append_identity_tag(bytes, 3);
        append_identity_bytes(bytes, &source.declaration_digest).ok()?;
        append_identity_tag(bytes, 4);
        append_identity_count(bytes, source.resources.len()).ok()?;
        for (key, value) in source.resources.iter() {
            append_identity_bytes(bytes, key.as_bytes()).ok()?;
            append_canonical_json_value(bytes, value).ok()?;
        }

        append_identity_tag(bytes, 5);
        append_identity_count(bytes, source.skills.len()).ok()?;
        for skill in &source.skills {
            append_identity_tag(bytes, 1);
            append_identity_bytes(bytes, skill.stable_id.as_bytes()).ok()?;
            append_identity_tag(bytes, 2);
            append_identity_bytes(bytes, skill.local_id.as_bytes()).ok()?;
            append_identity_tag(bytes, 3);
            let mut aliases = skill.aliases.clone();
            aliases.sort();
            append_identity_count(bytes, aliases.len()).ok()?;
            for alias in aliases {
                append_identity_bytes(bytes, alias.as_bytes()).ok()?;
            }
            append_identity_tag(bytes, 4);
            append_identity_bytes(bytes, skill.digest.as_bytes()).ok()?;
            append_identity_tag(bytes, 5);
            append_identity_bytes(bytes, skill.content.as_bytes()).ok()?;
            append_identity_tag(bytes, 6);
            append_skill_view_identity(bytes, std::slice::from_ref(&skill.entry))?;
        }

        append_identity_tag(bytes, 6);
        append_identity_count(bytes, source.exports.len()).ok()?;
        for export in &source.exports {
            append_identity_tag(bytes, 1);
            append_identity_bytes(bytes, export.declared_id.as_bytes()).ok()?;
            append_identity_tag(bytes, 2);
            append_identity_bytes(bytes, export.canonical_name.as_bytes()).ok()?;
            append_identity_tag(bytes, 3);
            append_tool_permission_identity(bytes, export.permission);
            append_identity_tag(bytes, 4);
            let mut aliases = export.aliases.clone();
            aliases.sort();
            append_identity_count(bytes, aliases.len()).ok()?;
            for alias in aliases {
                append_identity_bytes(bytes, alias.as_bytes()).ok()?;
            }
            append_identity_tag(bytes, 5);
            let expected = runtime_source_dispatch_identity(source, export).ok()?;
            let actual = tools.dispatch_identity_v1(&export.canonical_name)?;
            if actual != expected {
                return None;
            }
            append_identity_bytes(bytes, &expected).ok()?;
        }
    }
    Some(())
}

fn collect_bundle_tool_candidates(
    catalog: &BundleCatalog,
    bundle_id: &str,
    out: &mut BTreeMap<String, ResourceCandidate>,
) -> Result<(), BundleError> {
    let Some(resources) = catalog.bundle_resources(bundle_id, ExportKind::Tool) else {
        return Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: "bundle namespace".to_string(),
            reference: bundle_id.to_string(),
        });
    };
    for resource in resources {
        out.insert(
            resource.stable_id.clone(),
            ResourceCandidate::BundleLocal {
                short_name: resource.local_id.clone(),
                qualified_name: resource.stable_id.clone(),
                aliases: resource.aliases.clone(),
            },
        );
    }
    Ok(())
}

fn is_harness_source(source: &RuntimeSource, bundles: &BundleCatalog) -> bool {
    source.id.kind() != RuntimeSourceKind::Bundle
        || bundles.bundles().iter().any(|bundle| {
            bundle.identity().id == source.id.configured_id() && bundle.plugin_bundle().is_some()
        })
}

fn collect_harness_tool_candidates(
    plane: AgentToolPlane,
    basic_tools: &ToolRegistrySnapshot,
    full_tools: &ToolRegistrySnapshot,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    masks: &BTreeMap<String, String>,
    bundles: &BundleCatalog,
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    // `basic_tools` is the snapshot captured when the registry was built, before
    // any MCP or plugin publication. That is what makes it the internal public
    // plane: later contributions cannot reach it.
    let selected = match plane {
        AgentToolPlane::InternalPublic => basic_tools,
        AgentToolPlane::Full => full_tools,
    };
    let excluded_names = sources
        .values()
        .flat_map(|source| {
            source
                .exports
                .iter()
                .filter(|export| {
                    export.permission == ToolPermission::Mcp || !is_harness_source(source, bundles)
                })
                .map(|export| export.canonical_name.as_str())
        })
        .collect::<BTreeSet<_>>();
    let mut entries = selected.canonical_tools();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let pool: BTreeSet<String> = entries.iter().map(|(name, _)| name.clone()).collect();
    for (name, resolved) in entries {
        // MCP is an independent resource kind; do not re-home it under tool.
        if excluded_names.contains(name.as_str()) {
            continue;
        }
        // A masked tool is excluded from the view: the mask winner provides
        // the bare name instead. Masks only bite when the winner is visible
        // in this plane's pool, so a plane without the winner keeps the
        // built-in rather than losing the bare name entirely.
        if let Some(winner) = masks.get(name.as_str())
            && winner.as_str() != name.as_str()
            && pool.contains(winner)
        {
            continue;
        }
        // A mask winner is advertised under the bare name it won — wrapped so
        // the schema name is the bare spelling — and that spelling appears
        // exactly once (the qualified spelling is not also advertised).
        let promoted_bare = winner_bare_name(masks, name.as_str());
        let (short_name, resolved) = match promoted_bare {
            Some(bare) => (
                bare.clone(),
                ResolvedTool {
                    tool: Arc::new(NamedTool::new(bare.clone(), resolved.tool)),
                    permission: resolved.permission,
                },
            ),
            None => (name.clone(), resolved),
        };
        let qualified = harness_id("tool", &name);
        out.insert(
            qualified.clone(),
            ResourceCandidate::HarnessTool {
                short_name,
                qualified_name: qualified,
                resolved,
                aliases: selected.aliases_for_canonical(&name),
            },
        );
    }
}

/// The bare name `canonical` won through masking, when that differs from the
/// canonical spelling itself. Deterministic: the lexicographically smallest
/// contested name wins the primary spelling when one tool won several.
fn winner_bare_name(masks: &BTreeMap<String, String>, canonical: &str) -> Option<String> {
    masks
        .iter()
        .find(|(bare, winner)| winner.as_str() == canonical && bare.as_str() != canonical)
        .map(|(bare, _)| bare.clone())
}

fn collect_bundle_skill_candidates(
    catalog: &BundleCatalog,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    bundle_id: &str,
    out: &mut BTreeMap<String, ResourceCandidate>,
) -> Result<(), BundleError> {
    let Some(resources) = catalog.bundle_resources(bundle_id, ExportKind::Skill) else {
        return Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: "bundle namespace".to_string(),
            reference: bundle_id.to_string(),
        });
    };
    if resources.is_empty() {
        return Ok(());
    }
    let source_id = RuntimeSourceId::bundle(bundle_id);
    let Some(source) = sources.get(&source_id) else {
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: "prepared Skill contributions were not published".to_string(),
        });
    };
    let prepared = resources
        .iter()
        .map(|resource| (resource.stable_id.as_str(), resource))
        .collect::<BTreeMap<_, _>>();
    if prepared.len() != source.skills.len() {
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: "published Skill contributions do not match prepared resources".to_string(),
        });
    }
    for skill in &source.skills {
        let Some(resource) = prepared.get(skill.stable_id.as_str()) else {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!("published Skill `{}` is not prepared", skill.stable_id),
            });
        };
        if resource.local_id != skill.local_id
            || resource.digest != skill.digest
            || resource.aliases != skill.aliases
        {
            return Err(BundleError::InvalidManifest {
                source_name: bundle_id.to_string(),
                detail: format!(
                    "published Skill `{}` differs from prepared bytes",
                    skill.stable_id
                ),
            });
        }
        out.insert(
            skill.stable_id.clone(),
            ResourceCandidate::BundleLocalSkill {
                short_name: skill.local_id.clone(),
                qualified_name: skill.stable_id.clone(),
                entry: skill.entry.clone(),
                aliases: skill.aliases.clone(),
            },
        );
    }
    Ok(())
}

fn collect_harness_skill_candidates(
    plane: AgentToolPlane,
    harness_skills: &[SkillCatalogEntry],
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    bundles: &BundleCatalog,
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    // Project and user skills are discovered from the working directory. A
    // bundle agent must not see them. Agent-bearing bundle Skills remain
    // owner-scoped; only catalog-confirmed agentless Plugins join the Full plane.
    if plane != AgentToolPlane::Full {
        return;
    }
    for entry in harness_skills {
        let qualified = harness_id("skill", &entry.name);
        out.insert(
            qualified.clone(),
            ResourceCandidate::HarnessSkill {
                short_name: entry.name.clone(),
                qualified_name: qualified,
                entry: entry.clone(),
                aliases: Vec::new(),
            },
        );
    }
    for source in sources.values() {
        let agentless_bundle_plugin = source.id.kind() == RuntimeSourceKind::Bundle
            && bundles
                .bundles()
                .iter()
                .find(|bundle| bundle.identity().id == source.id.configured_id())
                .is_some_and(|bundle| bundle.plugin_bundle().is_some());
        if source.id.kind() != RuntimeSourceKind::Plugin && !agentless_bundle_plugin {
            continue;
        }
        for skill in &source.skills {
            if skill.stable_id.starts_with("bundle:") && !agentless_bundle_plugin {
                continue;
            }
            out.insert(
                skill.stable_id.clone(),
                ResourceCandidate::HarnessSkill {
                    short_name: skill.local_id.clone(),
                    qualified_name: skill.stable_id.clone(),
                    entry: skill.entry.clone(),
                    aliases: skill.aliases.clone(),
                },
            );
        }
    }
}

fn collect_bundle_mcp_candidates(
    catalog: &BundleCatalog,
    bundle_id: &str,
    out: &mut BTreeMap<String, ResourceCandidate>,
) -> Result<(), BundleError> {
    let Some(resources) = catalog.bundle_resources(bundle_id, ExportKind::Mcp) else {
        return Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: "bundle namespace".to_string(),
            reference: bundle_id.to_string(),
        });
    };
    for resource in resources {
        out.insert(
            resource.stable_id.clone(),
            ResourceCandidate::BundleLocal {
                short_name: resource.local_id.clone(),
                qualified_name: resource.stable_id.clone(),
                aliases: resource.aliases.clone(),
            },
        );
    }
    Ok(())
}

fn collect_harness_mcp_candidates(
    plane: AgentToolPlane,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    masks: &BTreeMap<String, String>,
    bundles: &BundleCatalog,
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    // MCP servers are configured at the Harness level. A bundle agent gets only
    // the MCP declarations its own bundle ships.
    if plane != AgentToolPlane::Full {
        return;
    }
    let mut exports = sources
        .values()
        .filter(|source| is_harness_source(source, bundles))
        .flat_map(|source| source.exports.iter())
        .filter(|export| export.permission == ToolPermission::Mcp)
        .collect::<Vec<_>>();
    exports.sort_by(|left, right| left.canonical_name.cmp(&right.canonical_name));
    for export in exports {
        // Contested aliases this export lost are dropped; its qualified
        // canonical spelling remains the escape hatch.
        let mut aliases = export
            .aliases
            .iter()
            .filter(|alias| {
                masks
                    .get(alias.as_str())
                    .is_none_or(|winner| winner.as_str() == export.canonical_name.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        // A mask winner is advertised under the bare name it won, exactly once.
        let (short_name, resolved) = match winner_bare_name(masks, &export.canonical_name) {
            Some(bare) => {
                aliases.retain(|alias| alias != &bare);
                (
                    bare.clone(),
                    ResolvedTool {
                        tool: Arc::new(NamedTool::new(bare.clone(), export.tool.clone())),
                        permission: export.permission,
                    },
                )
            }
            None => (
                export.canonical_name.clone(),
                ResolvedTool {
                    tool: export.tool.clone(),
                    permission: export.permission,
                },
            ),
        };
        let qualified = harness_id("mcp", &export.canonical_name);
        out.insert(
            qualified.clone(),
            ResourceCandidate::HarnessMcp {
                short_name,
                qualified_name: qualified,
                resolved,
                aliases,
            },
        );
    }
}

fn select_candidates_globally(
    policy: &AgentResourcePolicy,
    partitions: &CandidatePartitions,
) -> Result<SelectedIds, BundleError> {
    let bundle_id = policy.diagnostic_scope();
    let view = &policy.resource_view;
    let mut selected = SelectedIds {
        tool: partitions.tool.keys().cloned().collect(),
        skill: partitions.skill.keys().cloned().collect(),
        mcp: partitions.mcp.keys().cloned().collect(),
    };
    if !view.allow.is_empty() {
        let mut allowed = SelectedIds {
            tool: BTreeSet::new(),
            skill: BTreeSet::new(),
            mcp: BTreeSet::new(),
        };
        for reference in &view.allow {
            let hit = resolve_global_reference(policy, reference, partitions)?;
            match hit.kind {
                "tool" => {
                    allowed.tool.insert(hit.canonical);
                }
                "skill" => {
                    allowed.skill.insert(hit.canonical);
                }
                "mcp" => {
                    allowed.mcp.insert(hit.canonical);
                }
                other => {
                    return Err(BundleError::UnknownResourceReference {
                        bundle_id: bundle_id.to_string(),
                        kind: other.to_string(),
                        reference: reference.clone(),
                    });
                }
            }
        }
        selected.tool = selected.tool.intersection(&allowed.tool).cloned().collect();
        selected.skill = selected
            .skill
            .intersection(&allowed.skill)
            .cloned()
            .collect();
        selected.mcp = selected.mcp.intersection(&allowed.mcp).cloned().collect();
    }
    // Coordination tools are harness-owned: `allow` never narrows them away.
    let coordination = coordination_tool_ids(policy, partitions, &selected);
    selected.tool.extend(coordination);
    for reference in &view.deny {
        let hit = resolve_global_reference(policy, reference, partitions)?;
        match hit.kind {
            "tool" => {
                if let Some(name) = crate::coordination::UNDENIABLE_TOOLS
                    .iter()
                    .find(|name| hit.canonical == harness_id("tool", name))
                {
                    return Err(BundleError::InvalidManifest {
                        source_name: bundle_id.to_string(),
                        detail: format!(
                            "resource_view.deny `{reference}` removes the coordination tool \
                             `{name}`; a subagent without it can never finish (root agents \
                             never see it anyway)"
                        ),
                    });
                }
                selected.tool.remove(&hit.canonical);
            }
            "skill" => {
                selected.skill.remove(&hit.canonical);
            }
            "mcp" => {
                selected.mcp.remove(&hit.canonical);
            }
            other => {
                return Err(BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: other.to_string(),
                    reference: reference.clone(),
                });
            }
        }
    }
    // `task`/`archive` exist only for agents that can spawn someone, however
    // the view reached them (default view or an explicit allow).
    if !policy.spawn_rights {
        for name in crate::coordination::SPAWN_TOOLS {
            selected.tool.remove(&harness_id("tool", name));
        }
    }
    Ok(selected)
}

/// Harness coordination tool ids to inject into `policy`'s view (see
/// [`crate::coordination`]): each one present in the candidate pool, minus the
/// spawn tools without spawn rights, minus any whose bare name the view already
/// gives to one of its own selected resources or aliases (the bundle's own
/// resource keeps the name; nothing collides).
fn coordination_tool_ids(
    policy: &AgentResourcePolicy,
    partitions: &CandidatePartitions,
    selected: &SelectedIds,
) -> Vec<String> {
    crate::coordination::COORDINATION_TOOLS
        .iter()
        .filter(|name| policy.spawn_rights || !crate::coordination::SPAWN_TOOLS.contains(name))
        .filter_map(|name| {
            let id = harness_id("tool", name);
            if !partitions.tool.contains_key(&id) || selected.tool.contains(&id) {
                return None;
            }
            let claimed_by_view = policy.resource_view.aliases.contains_key(*name)
                || [
                    (&partitions.tool, &selected.tool),
                    (&partitions.mcp, &selected.mcp),
                ]
                .into_iter()
                .flat_map(|(candidates, chosen)| {
                    chosen.iter().filter_map(|chosen| candidates.get(chosen))
                })
                .any(|candidate| {
                    candidate.short_name() == *name
                        || candidate.aliases().iter().any(|alias| alias == name)
                });
            (!claimed_by_view).then_some(id)
        })
        .collect()
}

struct ResolvedReference {
    kind: &'static str,
    canonical: String,
}

fn resolve_global_reference(
    policy: &AgentResourcePolicy,
    reference: &str,
    partitions: &CandidatePartitions,
) -> Result<ResolvedReference, BundleError> {
    let bundle_id = policy.diagnostic_scope();
    if reference.starts_with("harness:") {
        let kind = kind_from_qualified_reference(reference).ok_or_else(|| {
            BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: "resource".to_string(),
                reference: reference.to_string(),
            }
        })?;
        if !matches!(kind, "tool" | "skill" | "mcp") {
            return Err(BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: kind.to_string(),
                reference: reference.to_string(),
            });
        }
        // The clamped plane admits no Harness skills and no Harness MCP. Say so
        // plainly instead of reporting the reference as unknown.
        if policy.plane == AgentToolPlane::InternalPublic && matches!(kind, "skill" | "mcp") {
            return Err(BundleError::ResourceNotInPlane {
                bundle_id: bundle_id.to_string(),
                reference: reference.to_string(),
                plane: policy.plane.as_str().to_string(),
            });
        }
        let candidates = partition_for(kind, partitions);
        if candidates.contains_key(reference) {
            return Ok(ResolvedReference {
                kind,
                canonical: reference.to_string(),
            });
        }
        return Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: kind.to_string(),
            reference: reference.to_string(),
        });
    }
    if reference.starts_with("bundle:") {
        let kind = kind_from_qualified_reference(reference).ok_or_else(|| {
            BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: "resource".to_string(),
                reference: reference.to_string(),
            }
        })?;
        if !matches!(kind, "tool" | "skill" | "mcp") {
            return Err(BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: kind.to_string(),
                reference: reference.to_string(),
            });
        }
        let candidates = partition_for(kind, partitions);
        if candidates.contains_key(reference) {
            return Ok(ResolvedReference {
                kind,
                canonical: reference.to_string(),
            });
        }
        return Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: kind.to_string(),
            reference: reference.to_string(),
        });
    }

    let mut matches = Vec::new();
    for (kind, candidates) in [
        ("tool", &partitions.tool),
        ("skill", &partitions.skill),
        ("mcp", &partitions.mcp),
    ] {
        for (id, candidate) in candidates {
            if candidate.short_name() == reference {
                matches.push((kind, id.clone()));
            }
        }
    }
    match matches.as_slice() {
        [] => Err(BundleError::UnknownResourceReference {
            bundle_id: bundle_id.to_string(),
            kind: "resource".to_string(),
            reference: reference.to_string(),
        }),
        [(kind, canonical)] => Ok(ResolvedReference {
            kind,
            canonical: canonical.clone(),
        }),
        _ => Err(BundleError::NamespaceCollision {
            bundle_id: bundle_id.to_string(),
            name: reference.to_string(),
        }),
    }
}

/// Extract the resource kind from a qualified harness or bundle reference.
/// Bundle IDs may themselves contain kind-like path segments, so parse
/// structurally from the rightmost `/{kind}/{local}` pair.
fn kind_from_qualified_reference(reference: &str) -> Option<&'static str> {
    if let Some(rest) = reference.strip_prefix("harness:") {
        let kind = rest.split_once('/')?.0;
        return normalize_kind(kind);
    }
    if let Some(rest) = reference.strip_prefix("bundle:") {
        // bundle:{bundle_id}/{kind}/{local_id} — split from the right twice.
        let mut parts = rest.rsplitn(3, '/');
        let local = parts.next()?;
        let kind = parts.next()?;
        let bundle_id = parts.next()?;
        if local.is_empty() || kind.is_empty() || bundle_id.is_empty() {
            return None;
        }
        return normalize_kind(kind);
    }
    None
}

fn normalize_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "tool" => "tool",
        "skill" => "skill",
        "mcp" => "mcp",
        "hook" => "hook",
        "agent" => "agent",
        _ => return None,
    })
}

fn partition_for<'a>(
    kind: &str,
    partitions: &'a CandidatePartitions,
) -> &'a BTreeMap<String, ResourceCandidate> {
    match kind {
        "skill" => &partitions.skill,
        "mcp" => &partitions.mcp,
        _ => &partitions.tool,
    }
}

/// Resolve every resource-view alias target once against all partitions.
fn resolve_view_aliases(
    policy: &AgentResourcePolicy,
    partitions: &CandidatePartitions,
    selected: &SelectedIds,
    view: &ResourceView,
) -> Result<Vec<(String, &'static str, String)>, BundleError> {
    let bundle_id = policy.diagnostic_scope();
    let mut out = Vec::new();
    for (alias, target) in &view.aliases {
        let hit = resolve_global_reference(policy, target, partitions)?;
        let selected_set = match hit.kind {
            "tool" => &selected.tool,
            "skill" => &selected.skill,
            "mcp" => &selected.mcp,
            other => {
                return Err(BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: other.to_string(),
                    reference: target.clone(),
                });
            }
        };
        if !selected_set.contains(&hit.canonical) {
            return Err(BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: hit.kind.to_string(),
                reference: target.clone(),
            });
        }
        out.push((alias.clone(), hit.kind, hit.canonical));
    }
    Ok(out)
}

fn aliases_for_kind(
    kind: &str,
    aliases: &[(String, &'static str, String)],
) -> BTreeMap<String, String> {
    aliases
        .iter()
        .filter(|(_, alias_kind, _)| *alias_kind == kind)
        .map(|(alias, _, canonical)| (alias.clone(), canonical.clone()))
        .collect()
}

fn reserve_stable_names(
    namespace: &str,
    kind: &str,
    candidates: &BTreeMap<String, ResourceCandidate>,
    sibling_dispatch: Option<&BTreeMap<String, ResourceCandidate>>,
) -> BTreeSet<String> {
    let mut reserved = BTreeSet::new();
    for candidate in candidates.values() {
        reserved.insert(candidate.short_name().to_string());
        reserved.insert(candidate.qualified_name().to_string());
        if candidate.is_bundle_local() {
            reserved.insert(namespace_qualified(namespace, kind, candidate.short_name()));
        }
    }
    if let Some(sibling) = sibling_dispatch {
        for candidate in sibling.values() {
            reserved.insert(candidate.short_name().to_string());
            reserved.insert(candidate.qualified_name().to_string());
        }
    }
    reserved
}

/// Select provider-safe canonical short names and explicit view aliases for schemas.
/// Qualified identities and candidate aliases remain dispatch-only spellings.
fn model_schema_public_names(
    bundle_id: &str,
    kind: &str,
    candidates: &BTreeMap<String, ResourceCandidate>,
    public: &BTreeMap<String, String>,
    view_aliases: &BTreeMap<String, String>,
) -> Result<BTreeSet<String>, BundleError> {
    let mut names = BTreeSet::new();
    let mut covered = BTreeSet::new();
    for (public_name, canonical_id) in public {
        let candidate =
            candidates
                .get(canonical_id)
                .ok_or_else(|| BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: canonical_id.clone(),
                })?;
        let explicit_alias = view_aliases.get(public_name) == Some(canonical_id);
        if public_name != candidate.short_name() && !explicit_alias {
            continue;
        }
        if !is_model_tool_name(public_name) {
            if explicit_alias {
                return Err(BundleError::InvalidManifest {
                    source_name: bundle_id.to_string(),
                    detail: format!(
                        "model-facing {kind} name `{public_name}` must be 1-64 ASCII letters, digits, `_`, or `-`"
                    ),
                });
            }
            continue;
        }
        names.insert(public_name.clone());
        covered.insert(canonical_id.clone());
    }
    for canonical_id in public.values() {
        if covered.contains(canonical_id) {
            continue;
        }
        return Err(BundleError::InvalidManifest {
            source_name: bundle_id.to_string(),
            detail: format!(
                "selected {kind} `{canonical_id}` has no provider-safe schema name; add an explicit alias using 1-64 ASCII letters, digits, `_`, or `-`"
            ),
        });
    }
    Ok(names)
}

/// Return whether a name satisfies the common provider Tool-name contract.
fn is_model_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn assign_public_names(
    bundle_id: &str,
    namespace: &str,
    kind: &str,
    candidates: &BTreeMap<String, ResourceCandidate>,
    selected: &BTreeSet<String>,
    view_aliases: &BTreeMap<String, String>,
    sibling_dispatch: Option<&BTreeMap<String, ResourceCandidate>>,
) -> Result<BTreeMap<String, String>, BundleError> {
    let mut short_owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for id in selected {
        let candidate =
            candidates
                .get(id)
                .ok_or_else(|| BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: id.clone(),
                })?;
        short_owners
            .entry(candidate.short_name().to_string())
            .or_default()
            .push(id.clone());
    }

    let mut reserved = reserve_stable_names(namespace, kind, candidates, sibling_dispatch);

    let mut ordinary_short: BTreeMap<String, String> = BTreeMap::new();
    for id in selected {
        let candidate =
            candidates
                .get(id)
                .ok_or_else(|| BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: id.clone(),
                })?;
        let short = candidate.short_name();
        let owners = short_owners.get(short).map_or(&[][..], Vec::as_slice);
        let gets_short = if owners.len() == 1 {
            true
        } else {
            let local = owners
                .iter()
                .filter_map(|owner| candidates.get(owner))
                .filter(|candidate| candidate.is_bundle_local())
                .count();
            let harness = owners.len() - local;
            if local > 1 || harness > 1 {
                return Err(BundleError::NamespaceCollision {
                    bundle_id: bundle_id.to_string(),
                    name: short.to_string(),
                });
            }
            candidate.is_bundle_local()
        };
        if gets_short {
            ordinary_short.insert(id.clone(), short.to_string());
        }
    }

    let mut public = BTreeMap::new();
    for id in selected {
        let candidate =
            candidates
                .get(id)
                .ok_or_else(|| BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: id.clone(),
                })?;
        if let Some(short) = ordinary_short.get(id)
            && public.insert(short.clone(), id.clone()).is_some()
        {
            return Err(BundleError::NamespaceCollision {
                bundle_id: bundle_id.to_string(),
                name: short.clone(),
            });
        }
        let qualified_public = if candidate.is_bundle_local() {
            namespace_qualified(namespace, kind, candidate.short_name())
        } else {
            candidate.qualified_name().to_string()
        };
        if public
            .insert(qualified_public.clone(), id.clone())
            .is_some_and(|existing| existing != *id)
        {
            return Err(BundleError::NamespaceCollision {
                bundle_id: bundle_id.to_string(),
                name: qualified_public,
            });
        }
    }

    for (alias, canonical) in view_aliases {
        if !selected.contains(canonical) {
            return Err(BundleError::UnknownResourceReference {
                bundle_id: bundle_id.to_string(),
                kind: kind.to_string(),
                reference: canonical.clone(),
            });
        }
        if reserved.contains(alias) {
            return Err(BundleError::AliasCollision {
                bundle_id: bundle_id.to_string(),
                name: alias.clone(),
            });
        }
        // Kind-local: explicit aliases collide with preexisting candidate
        // aliases (registry/source/prepared), even for the same target.
        if candidates
            .values()
            .any(|candidate| candidate.aliases().iter().any(|existing| existing == alias))
        {
            return Err(BundleError::AliasCollision {
                bundle_id: bundle_id.to_string(),
                name: alias.clone(),
            });
        }
        if public.get(alias).is_some_and(|id| id != canonical) {
            return Err(BundleError::AliasCollision {
                bundle_id: bundle_id.to_string(),
                name: alias.clone(),
            });
        }
        if let Some(short) = ordinary_short.get(canonical)
            && public.get(short).is_some_and(|id| id == canonical)
        {
            public.remove(short);
        }
        public.insert(alias.clone(), canonical.clone());
        reserved.insert(alias.clone());
    }

    Ok(public)
}

/// Project catalog/snapshot/source aliases into the compiled public map for
/// selected candidates. These are additive public spellings, not allow/deny
/// identities. When a canonical identity has one or more explicit resource-view
/// aliases, all of its candidate effective aliases are suppressed so prior
/// short/registry aliases cannot bypass the mapping.
fn inject_effective_aliases(
    bundle_id: &str,
    kind: &str,
    candidates: &BTreeMap<String, ResourceCandidate>,
    selected: &BTreeSet<String>,
    view_aliases: &BTreeMap<String, String>,
    public: &mut BTreeMap<String, String>,
) -> Result<(), BundleError> {
    let explicitly_aliased = view_aliases.values().cloned().collect::<BTreeSet<_>>();
    for id in selected {
        if explicitly_aliased.contains(id) {
            continue;
        }
        let candidate =
            candidates
                .get(id)
                .ok_or_else(|| BundleError::UnknownResourceReference {
                    bundle_id: bundle_id.to_string(),
                    kind: kind.to_string(),
                    reference: id.clone(),
                })?;
        for alias in candidate.aliases() {
            if let Some(existing) = public.get(alias) {
                if existing != id {
                    return Err(BundleError::AliasCollision {
                        bundle_id: bundle_id.to_string(),
                        name: alias.clone(),
                    });
                }
                continue;
            }
            public.insert(alias.clone(), id.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use async_trait::async_trait;
    use hya_bundle::{
        AgentRole, BundleIdentity, BundleSource, ModelPolicy, PreparedAgent, PreparedAgentBundle,
        PreparedInstallableBundle, PreparedResource, SourceFile, prepare_package,
    };
    use hya_proto::{AgentName, ToolName};
    use hya_tool::{
        Action, InvocationPolicy, InvocationRule, Mode, PermissionModel, PermissionPlane,
        PermissionRules, PermissionTarget, Rule, Tool, ToolCtx, ToolError, ToolPermission,
        ToolRegistry,
    };
    use serde_json::{Value, json};
    use std::path::PathBuf;

    /// Test shim: wrap prepared AgentBundles as an [`AgentCatalog`] over the
    /// compiled-in built-in roster, so unit tests keep their existing shape.
    struct TestCatalog;

    impl TestCatalog {
        fn from_prepared(bundles: &[PreparedAgentBundle]) -> Result<AgentCatalog, BundleError> {
            let bundles = bundles
                .iter()
                .cloned()
                .map(|bundle| PreparedInstallableBundle::Agent(Box::new(bundle)))
                .collect::<Vec<_>>();
            AgentCatalog::new(Arc::new(BundleCatalog::from_prepared(&bundles)?))
        }

        fn from_verified_catalogs(
            catalogs: &[&hya_bundle::PreparedCatalog],
        ) -> Result<AgentCatalog, BundleError> {
            AgentCatalog::new(Arc::new(BundleCatalog::from_verified_catalogs(catalogs)?))
        }
    }

    /// Build a test registry and publish every prepared bundle Skill through the runtime source seam.
    fn test_runtime_registry(tools: ToolRegistry, catalog: Arc<AgentCatalog>) -> RuntimeRegistry {
        let sources = catalog
            .bundles()
            .bundles()
            .iter()
            .filter_map(|bundle| {
                let skills = bundle
                    .resources(hya_bundle::ExportKind::Skill)
                    .iter()
                    .map(|resource| {
                        let parsed = hya_tool::parse_skill(&resource.content)
                            .expect("test bundle Skill must contain valid frontmatter");
                        let path = PathBuf::from(format!(
                            "bundle:{}/{}",
                            bundle.identity().id,
                            resource.source_path
                        ));
                        let dir = path
                            .parent()
                            .expect("test bundle Skill path must have a parent")
                            .to_path_buf();
                        RuntimeSourceSkill::new(
                            resource.stable_id.clone(),
                            resource.local_id.clone(),
                            resource.aliases.clone(),
                            resource.digest.clone(),
                            resource.content.clone(),
                            SkillCatalogEntry {
                                name: parsed.name,
                                description: parsed.description,
                                content: parsed.content,
                                allowed_tools: parsed.allowed_tools,
                                model: parsed.model,
                                path,
                                dir,
                                origin: hya_tool::SkillCatalogOrigin::Virtual,
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                (!skills.is_empty()).then(|| {
                    RuntimeSource::new(
                        RuntimeSourceId::bundle(bundle.identity().id.clone()),
                        [0; 32],
                        Arc::new(()),
                        Vec::new(),
                    )
                    .with_skills(skills)
                })
            })
            .collect::<Vec<_>>();
        let registry = RuntimeRegistry::new(tools, catalog);
        if !sources.is_empty() {
            registry
                .refresh(|candidate| {
                    candidate.replace_sources_of_kind(RuntimeSourceKind::Bundle, sources)
                })
                .expect("test bundle Skill sources must publish");
        }
        registry
    }

    #[test]
    fn full_view_exposes_only_catalog_confirmed_plugin_bundle_skills() {
        let prepare = |kind: &str, id: &str, name: &str, agent: &str| {
            prepare_package(BundleSource::new(
                id,
                vec![
                    SourceFile::new("bundle.yaml", format!(
                        "kind: {kind}\nidentity: {{ id: acme/{id}, version: 1.0.0, publisher: acme }}\nresources:\n  skills:\n    - id: {name}\n      path: skills/guide.md\n{agent}"
                    )),
                    SourceFile::new("skills/guide.md", skill_md(name, "SKILL_BODY")),
                ],
            )).unwrap()
        };
        let plugin = prepare("Plugin", "shared", "shared-skill", "");
        let agent = prepare(
            "AgentBundle",
            "private",
            "private-skill",
            "agent: { id: private-agent, role: main }\n",
        );
        let catalog = Arc::new(TestCatalog::from_verified_catalogs(&[&plugin, &agent]).unwrap());
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        // An unrecognized bundle source cannot opt into shared Skill visibility.
        let mut unknown =
            registry.active().sources[&RuntimeSourceId::bundle("acme/shared")].clone();
        unknown.id = RuntimeSourceId::bundle("acme/unknown");
        unknown.skills[0].stable_id = "bundle:acme/unknown/skill/unknown-skill".into();
        unknown.skills[0].local_id = "unknown-skill".into();
        unknown.skills[0].entry.name = "unknown-skill".into();
        registry
            .refresh(|candidate| candidate.upsert_sources(vec![unknown]))
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-plugin-skill-view"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("build", AgentToolPlane::Full)
            .unwrap();
        let resources = binding.compile_agent_resources(&policy).unwrap();
        assert!(
            resources
                .skills()
                .iter()
                .any(|skill| skill.name == "shared-skill")
        );
        assert!(
            resources
                .skills()
                .iter()
                .any(|skill| skill.name == "bundle:acme/shared/skill/shared-skill")
        );
        assert!(
            !resources
                .skills()
                .iter()
                .any(|skill| skill.name == "private-skill")
        );
        assert!(
            !resources
                .skills()
                .iter()
                .any(|skill| skill.name == "unknown-skill")
        );
        let private_policy = binding.agent_resource_policy("private-agent").unwrap();
        let private_resources = binding.compile_agent_resources(&private_policy).unwrap();
        assert!(
            private_resources
                .skills()
                .iter()
                .any(|skill| skill.name == "private-skill")
        );
        assert!(
            !private_resources
                .skills()
                .iter()
                .any(|skill| skill.name == "shared-skill")
        );
    }

    #[test]
    fn bundled_mcp_stays_in_mcp_partition() {
        let prepared = prepare_package(BundleSource::new("bundled-mcp", vec![
            SourceFile::new("bundle.yaml", "kind: Plugin\nidentity: { id: acme/bundled-mcp, version: 1.0.0, publisher: acme }\n"),
        ])).unwrap();
        let catalog = Arc::new(TestCatalog::from_verified_catalogs(&[&prepared]).unwrap());
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![RuntimeSource::new(
                    RuntimeSourceId::bundle("acme/bundled-mcp"),
                    [1; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "mcp/echo/ping",
                        "bundled-mcp__mcp__echo__ping",
                        Vec::new(),
                        Arc::new(NoopTool::new("bundled-mcp__mcp__echo__ping")),
                        ToolPermission::Mcp,
                    )],
                )])
            })
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-bundled-mcp-kind"))
            .unwrap();
        let policy = binding.agent_resource_policy("build").unwrap();
        let candidates = binding.collect_resource_candidates(&policy).unwrap();
        assert!(
            candidates
                .mcp
                .contains_key("harness:mcp/bundled-mcp__mcp__echo__ping")
        );
        assert!(
            !candidates
                .tool
                .contains_key("harness:tool/bundled-mcp__mcp__echo__ping")
        );
    }

    #[test]
    fn bundle_process_tool_is_owner_scoped_and_needs_no_bun_sidecar() {
        let prepared = prepare_package(BundleSource::new(
            "private-process",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    r#"kind: AgentBundle
identity: { id: acme/private-process, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [fixture] }
resources:
  tools: [{ id: echo, path: echo.json }]
  hooks: [{ id: tool.execute.before, path: echo.json }]
agent:
  hook_refs: [tool.execute.before]
  id: private-process-agent
  role: main
  resource_view: { allow: [echo] }
"#,
                ),
                SourceFile::new("echo.json", "{}"),
            ],
        ))
        .unwrap();
        let catalog = Arc::new(TestCatalog::from_verified_catalogs(&[&prepared]).unwrap());
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![
                    RuntimeSource::new(
                        RuntimeSourceId::bundle("acme/private-process"),
                        [2; 32],
                        Arc::new(()),
                        vec![RuntimeSourceExport::tool(
                            "echo",
                            "private-process__echo",
                            Vec::new(),
                            Arc::new(NoopTool::new("private-process__echo")),
                            ToolPermission::Tool,
                        )],
                    )
                    .with_hooks(Arc::new(crate::hooks::HookChain::new(Vec::new()))),
                ])
            })
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-private-process"))
            .unwrap();
        let policy = binding
            .agent_resource_policy("private-process-agent")
            .unwrap();
        let compiled = binding
            .compile_agent_resources(&policy)
            .expect("owner process resource must compile");
        assert!(compiled.resolve_tool("echo").is_some());
        assert_eq!(
            binding
                .bundle_hooks_for_agent("private-process-agent")
                .len(),
            1
        );
        assert!(binding.bundle_hooks_for_agent("build").is_empty());
        assert!(
            !binding
                .has_selected_bundle_sidecar_capability("private-process-agent")
                .unwrap()
        );
        let root = binding
            .compile_agent_resources(&binding.agent_resource_policy("build").unwrap())
            .unwrap();
        assert!(root.resolve_tool("private-process__echo").is_none());
    }

    /// Installed Plugin-kind bundle hooks reach bundle agents too: a bundle
    /// agent's chain is every Plugin's hooks (source-id order, the very same
    /// retained dispatchers built-in agents get) followed by its own bundle's
    /// `hook_refs`-scoped hooks. An agent without `hook_refs` still gets the
    /// Plugin hooks.
    #[test]
    fn bundle_agents_receive_installed_plugin_hooks_before_their_own() {
        let plugin_b = prepare_package(BundleSource::new(
            "plugin-b",
            vec![SourceFile::new(
                "bundle.yaml",
                "kind: Plugin\nidentity: { id: acme/plugin-b, version: 1.0.0, publisher: acme }\n",
            )],
        ))
        .unwrap();
        let plugin_a = prepare_package(BundleSource::new(
            "plugin-a",
            vec![SourceFile::new(
                "bundle.yaml",
                "kind: Plugin\nidentity: { id: acme/plugin-a, version: 1.0.0, publisher: acme }\n",
            )],
        ))
        .unwrap();
        let agents = prepare_package(BundleSource::new(
            "owner",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    r#"kind: AgentSetBundle
identity: { id: acme/owner, version: 1.0.0, publisher: acme }
extensions:
  process: { kind: rust, command: [fixture] }
resources:
  hooks: [{ id: chat.params, path: hook.json }]
agents:
  - id: hooked-agent
    role: main
    hook_refs: [chat.params]
  - id: plain-agent
    role: main
"#,
                ),
                SourceFile::new("hook.json", "{}"),
            ],
        ))
        .unwrap();
        let catalog = Arc::new(
            TestCatalog::from_verified_catalogs(&[&plugin_b, &agents, &plugin_a]).unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let empty = || -> Arc<dyn crate::hooks::HookDispatcher> {
            Arc::new(crate::hooks::HookChain::new(Vec::new()))
        };
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![
                    RuntimeSource::new(
                        RuntimeSourceId::bundle("acme/plugin-b"),
                        [4; 32],
                        Arc::new(()),
                        Vec::new(),
                    )
                    .with_hooks(empty()),
                    RuntimeSource::new(
                        RuntimeSourceId::bundle("acme/owner"),
                        [5; 32],
                        Arc::new(()),
                        Vec::new(),
                    )
                    .with_hooks(empty()),
                    RuntimeSource::new(
                        RuntimeSourceId::bundle("acme/plugin-a"),
                        [6; 32],
                        Arc::new(()),
                        Vec::new(),
                    )
                    .with_hooks(empty()),
                ])
            })
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-plugin-hooks-bundle-agents"))
            .unwrap();

        let plugin_a_hooks = binding.bundle_hooks("acme/plugin-a").unwrap();
        let plugin_b_hooks = binding.bundle_hooks("acme/plugin-b").unwrap();
        let owner_hooks = binding.bundle_hooks("acme/owner").unwrap();

        let builtin = binding.bundle_hooks_for_agent("build");
        assert_eq!(builtin.len(), 2, "built-ins keep every Plugin's hooks");
        assert!(Arc::ptr_eq(&builtin[0], &plugin_a_hooks));
        assert!(Arc::ptr_eq(&builtin[1], &plugin_b_hooks));

        let hooked = binding.bundle_hooks_for_agent("hooked-agent");
        assert_eq!(
            hooked.len(),
            3,
            "Plugin hooks, then the owner's scoped hooks"
        );
        assert!(Arc::ptr_eq(&hooked[0], &plugin_a_hooks));
        assert!(Arc::ptr_eq(&hooked[1], &plugin_b_hooks));
        assert!(
            !Arc::ptr_eq(&hooked[2], &owner_hooks),
            "the owner's hooks stay filtered by hook_refs"
        );

        let plain = binding.bundle_hooks_for_agent("plain-agent");
        assert_eq!(plain.len(), 2, "no hook_refs still gets the Plugin hooks");
        assert!(Arc::ptr_eq(&plain[0], &plugin_a_hooks));
        assert!(Arc::ptr_eq(&plain[1], &plugin_b_hooks));
        assert!(binding.bundle_hooks_for_agent("missing-agent").is_empty());
    }

    #[test]
    fn bundle_mcp_server_selection_expands_owner_tools_only() {
        let prepared = prepare_package(BundleSource::new(
            "private-mcp",
            vec![
                SourceFile::new(
                    "bundle.yaml",
                    r#"kind: AgentBundle
identity: { id: acme/private-mcp, version: 1.0.0, publisher: acme }
resources:
  mcp: [{ id: echo, path: mcp.json }]
agent:
  id: private-mcp-agent
  role: main
  resource_view: { allow: [echo] }
"#,
                ),
                SourceFile::new("mcp.json", r#"{"command":["fixture"]}"#),
            ],
        ))
        .unwrap();
        let catalog = Arc::new(TestCatalog::from_verified_catalogs(&[&prepared]).unwrap());
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![RuntimeSource::new(
                    RuntimeSourceId::bundle("acme/private-mcp"),
                    [3; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "mcp/echo/ping",
                        "private-mcp__mcp__echo__ping",
                        Vec::new(),
                        Arc::new(NoopTool::new("private-mcp__mcp__echo__ping")),
                        ToolPermission::Mcp,
                    )],
                )])
            })
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-private-mcp"))
            .unwrap();
        let policy = binding.agent_resource_policy("private-mcp-agent").unwrap();
        let compiled = binding
            .compile_agent_resources(&policy)
            .expect("selected MCP server must provide its tools");
        assert_eq!(
            compiled.resolve_tool("echo__ping").unwrap().permission,
            ToolPermission::Mcp
        );
        assert_eq!(
            domain_schema_names(&compiled),
            BTreeSet::from(["echo__ping".to_string()])
        );
        let root = binding
            .compile_agent_resources(&binding.agent_resource_policy("build").unwrap())
            .unwrap();
        assert!(root.resolve_tool("private-mcp__mcp__echo__ping").is_none());
    }

    struct NoopTool {
        name: String,
    }

    impl NoopTool {
        fn new(name: impl Into<String>) -> Self {
            Self { name: name.into() }
        }
    }

    struct FingerprintTool {
        name: String,
        description: String,
        input_schema: Value,
    }

    impl FingerprintTool {
        fn new(name: impl Into<String>, marker: &str) -> Self {
            Self {
                name: name.into(),
                description: format!("fingerprint schema {marker}"),
                input_schema: json!({
                    "type": "object",
                    "properties": {"marker": {"const": marker}},
                }),
            }
        }
    }

    #[async_trait]
    impl Tool for FingerprintTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new(self.name.clone()),
                description: self.description.clone(),
                input_schema: self.input_schema.clone(),
                output_schema: None,
            }
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
            Ok(json!({"ok": true}))
        }
    }

    #[async_trait]
    impl Tool for NoopTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new(self.name.clone()),
                description: "noop".to_string(),
                input_schema: json!({ "type": "object" }),
                output_schema: None,
            }
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
            Ok(json!({ "ok": true }))
        }
    }

    fn skill_md(name: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: {name}\n---\n{body}\n")
    }

    fn bundle_with_agent(
        bundle_id: &str,
        agent: PreparedAgent,
        skills: Vec<PreparedResource>,
    ) -> PreparedAgentBundle {
        PreparedAgentBundle {
            format_version: 2,
            identity: BundleIdentity {
                id: bundle_id.to_string(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            namespace: None,
            digest: "test-only".to_string(),
            agent,
            tools: Vec::new(),
            skills,
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
        }
    }

    fn agent(stable_id: &str, resource_view: ResourceView) -> PreparedAgent {
        PreparedAgent {
            id: AgentName::new(stable_id),
            description: None,
            role: AgentRole::Main,
            color: None,
            prompt: Some("prompt".to_string()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            legacy_spawn_lifecycle: None,
            resource_view,
            can_spawn: Vec::new(),
            hook_refs: Vec::new(),
        }
    }

    #[test]
    fn pure_registry_serves_builtin_skills_only() {
        let dir = std::env::temp_dir().join(format!("hya-pure-skills-{}", std::process::id()));
        let skill_dir = dir.join(".hya/skills/external-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: external-skill\ndescription: must not appear in pure mode\n---\nbody\n",
        )
        .unwrap();

        let catalog = Arc::new(TestCatalog::from_prepared(&[]).unwrap());
        let plain = test_runtime_registry(ToolRegistry::builtins(), catalog.clone());
        let pure = test_runtime_registry(ToolRegistry::builtins(), catalog).with_pure_skills(true);

        let plain_binding = plain.bind_turn(&dir).unwrap();
        let plain_names: Vec<String> = plain_binding
            .snapshot
            .skills
            .get(&dir)
            .map(|skills| skills.iter().map(|skill| skill.name.clone()).collect())
            .unwrap_or_default();
        assert!(
            plain_names.iter().any(|name| name == "external-skill"),
            "default registry discovers the project skill: {plain_names:?}"
        );

        let pure_binding = pure.bind_turn(&dir).unwrap();
        let pure_names: Vec<String> = pure_binding
            .snapshot
            .skills
            .get(&dir)
            .map(|skills| skills.iter().map(|skill| skill.name.clone()).collect())
            .unwrap_or_default();
        assert!(
            !pure_names.iter().any(|name| name == "external-skill"),
            "pure registry never reads external skill dirs: {pure_names:?}"
        );
        assert!(
            !pure_names.is_empty(),
            "pure registry still serves the embedded builtin catalog"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_turn_binding_pins_tools_across_later_publication() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/pin",
                agent("pin-agent", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let workdir = PathBuf::from("/tmp/hya-resource-view-pin");
        let binding = registry.bind_turn(&workdir).unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("pin-agent", AgentToolPlane::Full)
            .unwrap();
        let before = binding.compile_agent_resources(&policy).unwrap();
        assert!(before.resolve_tool("read").is_some());
        assert!(before.resolve_tool("dynamic_marker").is_none());

        registry
            .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("dynamic_marker"))))
            .unwrap();

        let after = binding.compile_agent_resources(&policy).unwrap();
        assert!(
            after.resolve_tool("dynamic_marker").is_none(),
            "retained TurnBinding must not observe later registry publication"
        );
        assert_eq!(
            before.public_tool_names(),
            after.public_tool_names(),
            "pinned binding must compile an identical public tool set"
        );
    }

    #[test]
    fn source_name_registry_rejects_bare_plugin_tool_names() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/name-registry",
                agent("name-registry", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let error = registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![RuntimeSource::new(
                    RuntimeSourceId::plugin("bare-tools"),
                    [9; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "read",
                        "read",
                        Vec::new(),
                        Arc::new(NoopTool::new("read")),
                        ToolPermission::Tool,
                    )],
                )])
            })
            .expect_err("bare plugin tool names must be rejected");
        let RuntimeRefreshError::NamingConflicts(report) = error else {
            panic!("expected a naming conflict report: {error:?}");
        };
        assert!(
            report.contains("plugin:bare-tools") && report.contains("`read`"),
            "report must name the source and the bare name: {report}"
        );
    }

    #[test]
    fn source_name_registry_groups_cross_source_duplicates() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/name-registry",
                agent("name-registry", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let error = registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![
                    RuntimeSource::new(
                        RuntimeSourceId::plugin("dup-a"),
                        [10; 32],
                        Arc::new(()),
                        vec![RuntimeSourceExport::tool(
                            "dup__tool",
                            "dup__tool",
                            Vec::new(),
                            Arc::new(NoopTool::new("dup__tool")),
                            ToolPermission::Tool,
                        )],
                    ),
                    RuntimeSource::new(
                        RuntimeSourceId::plugin("dup-b"),
                        [11; 32],
                        Arc::new(()),
                        vec![RuntimeSourceExport::tool(
                            "dup__tool",
                            "dup__tool",
                            Vec::new(),
                            Arc::new(NoopTool::new("dup__tool")),
                            ToolPermission::Tool,
                        )],
                    ),
                ])
            })
            .expect_err("cross-source duplicates must be rejected");
        let RuntimeRefreshError::NamingConflicts(report) = error else {
            panic!("expected a naming conflict report: {error:?}");
        };
        assert!(
            report.contains("plugin:dup-a") && report.contains("plugin:dup-b"),
            "report must list every conflicting source: {report}"
        );
    }

    #[test]
    fn alias_masking_publishes_and_compiles_bare_name_to_winner() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/mask",
                agent("mask-agent", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![RuntimeSource::new(
                    RuntimeSourceId::plugin("mask-src"),
                    [14; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "fast",
                        "mask__fast",
                        vec!["bash".to_string()],
                        Arc::new(NoopTool::new("mask__fast")),
                        ToolPermission::Tool,
                    )],
                )])
            })
            .expect("alias masking a built-in must publish");

        let workdir = PathBuf::from("/tmp/hya-alias-mask-builtin");
        let binding = registry.bind_turn(&workdir).unwrap();
        assert_eq!(
            binding.masks().get("bash").map(String::as_str),
            Some("mask__fast"),
            "the contributed alias must win the bare name from the built-in"
        );
        let chain = binding.mask_chain("bash");
        assert_eq!(
            chain.first().map(|(owner, _)| owner.as_str()),
            Some("built-in")
        );
        assert_eq!(
            chain.last(),
            Some(&("plugin:mask-src".to_string(), "mask__fast".to_string())),
            "the mask chain must end at the winning source claim"
        );

        let policy = binding
            .agent_resource_policy_on_plane("mask-agent", AgentToolPlane::Full)
            .unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        let names = compiled.public_tool_names();
        assert!(
            names.contains("bash") && !names.contains("harness:tool/bash"),
            "the masked built-in must be excluded while the bare name stays: {names:?}"
        );
        assert!(
            names.contains("harness:tool/mask__fast"),
            "the winning contributed tool must stay in the view: {names:?}"
        );
        let bash_schemas = compiled
            .tool_schemas()
            .into_iter()
            .filter(|schema| schema.name.as_str() == "bash")
            .collect::<Vec<_>>();
        assert_eq!(
            bash_schemas.len(),
            1,
            "the bare name must appear exactly once"
        );
        assert_eq!(
            bash_schemas[0].input_schema,
            json!({ "type": "object" }),
            "the advertised bash schema must be the contributed winner, not the built-in"
        );
        assert!(compiled.resolve_tool("bash").is_some());

        let mut removed = BTreeSet::new();
        removed.insert(RuntimeSourceId::plugin("mask-src"));
        registry
            .refresh(|candidate| {
                candidate.remove_sources(&removed);
                Ok(())
            })
            .expect("source removal must publish");

        let binding = registry.bind_turn(&workdir).unwrap();
        assert!(
            binding.masks().is_empty(),
            "removing the winner must dissolve the mask"
        );
        let policy = binding
            .agent_resource_policy_on_plane("mask-agent", AgentToolPlane::Full)
            .unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        let names = compiled.public_tool_names();
        assert!(
            names.contains("harness:tool/bash"),
            "the built-in must be resolvable again after the masking source is removed: {names:?}"
        );
    }

    #[test]
    fn protected_read_alias_is_a_hard_error() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/name-registry",
                agent("name-registry", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let error = registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![RuntimeSource::new(
                    RuntimeSourceId::plugin("read-masker"),
                    [15; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "reader",
                        "read_masker__reader",
                        vec!["read".to_string()],
                        Arc::new(NoopTool::new("read_masker__reader")),
                        ToolPermission::Tool,
                    )],
                )])
            })
            .expect_err("no source may mask the protected `read` tool");
        let RuntimeRefreshError::NamingConflicts(report) = error else {
            panic!("expected a naming conflict report: {error:?}");
        };
        assert!(
            report.contains("protected tool `read` cannot be masked"),
            "report must name the read protection: {report}"
        );
    }

    #[test]
    fn cross_source_alias_mask_goes_to_greater_source_id() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/mask",
                agent("mask-agent", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let contender = |configured_id: &str, digest: [u8; 32], canonical: &str| {
            RuntimeSource::new(
                RuntimeSourceId::plugin(configured_id),
                digest,
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "lookup",
                    canonical,
                    vec!["lookup".to_string()],
                    Arc::new(NoopTool::new(canonical)),
                    ToolPermission::Tool,
                )],
            )
        };
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![
                    contender("alpha", [16; 32], "alpha__lookup"),
                    contender("beta", [17; 32], "beta__lookup"),
                ])
            })
            .expect("cross-source alias collisions must mask instead of rejecting");

        let workdir = PathBuf::from("/tmp/hya-alias-mask-cross-source");
        let binding = registry.bind_turn(&workdir).unwrap();
        assert_eq!(
            binding.masks().get("lookup").map(String::as_str),
            Some("beta__lookup"),
            "the lexicographically greater source id must win the bare name"
        );
        let policy = binding
            .agent_resource_policy_on_plane("mask-agent", AgentToolPlane::Full)
            .unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        let names = compiled.public_tool_names();
        assert!(
            names.contains("lookup"),
            "the contested bare name must resolve in the view: {names:?}"
        );
        assert!(
            names.contains("harness:tool/beta__lookup"),
            "the winner keeps its qualified identity: {names:?}"
        );
        assert!(
            names.contains("alpha__lookup"),
            "the loser must stay resolvable via its qualified canonical: {names:?}"
        );
        assert!(
            compiled.resolve_tool("alpha__lookup").is_some(),
            "the loser's qualified canonical is the escape hatch"
        );
        let lookup_schemas = compiled
            .tool_schemas()
            .into_iter()
            .filter(|schema| schema.name.as_str() == "lookup")
            .collect::<Vec<_>>();
        assert_eq!(
            lookup_schemas.len(),
            1,
            "the bare name must appear exactly once"
        );
        assert_eq!(
            lookup_schemas[0].input_schema,
            json!({ "type": "object" }),
            "the advertised lookup schema must be the winner's tool"
        );
        assert!(
            !compiled
                .tool_schemas()
                .iter()
                .any(|schema| schema.name.as_str() == "beta__lookup"),
            "a masked-in winner must not also advertise its qualified spelling"
        );
    }

    #[test]
    fn masks_accessor_reports_only_contested_bare_names() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/mask",
                agent("mask-agent", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![RuntimeSource::new(
                    RuntimeSourceId::plugin("acc-src"),
                    [18; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "acc",
                        "acc__tool",
                        vec!["solo".to_string(), "grep".to_string()],
                        Arc::new(NoopTool::new("acc__tool")),
                        ToolPermission::Tool,
                    )],
                )])
            })
            .expect("mixed contested and uncontested aliases must publish");

        let workdir = PathBuf::from("/tmp/hya-mask-table");
        let binding = registry.bind_turn(&workdir).unwrap();
        let mut expected = BTreeMap::new();
        expected.insert("grep".to_string(), "acc__tool".to_string());
        assert_eq!(
            binding.masks(),
            &expected,
            "only the built-in contest belongs in the mask table"
        );
        assert!(
            binding.resolve_tool("solo").is_some(),
            "an uncontested alias keeps working through the registry"
        );
    }

    #[test]
    fn runtime_source_dispatch_identity_tracks_authoritative_source_semantics() {
        let source_identity =
            |configured_id: &str, declaration_digest: [u8; 32], resource_value: Value| {
                let catalog = Arc::new(
                    TestCatalog::from_prepared(&[bundle_with_agent(
                        "hya/source-identity",
                        agent("source-identity", ResourceView::default()),
                        Vec::new(),
                    )])
                    .unwrap(),
                );
                let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
                let mut resources = BTreeMap::new();
                resources.insert("probe".to_string(), resource_value);
                registry
                    .refresh(|candidate| {
                        candidate.upsert_sources(vec![
                            RuntimeSource::new(
                                RuntimeSourceId::plugin(configured_id),
                                declaration_digest,
                                Arc::new(()),
                                vec![RuntimeSourceExport::tool(
                                    "probe",
                                    "plugin__fixture__probe",
                                    Vec::new(),
                                    Arc::new(NoopTool::new("plugin__fixture__probe")),
                                    ToolPermission::Tool,
                                )],
                            )
                            .with_resources(resources),
                        ])
                    })
                    .unwrap();
                let identity = registry
                    .active()
                    .tools
                    .dispatch_identity_v1("plugin__fixture__probe");
                let Some(identity) = identity else {
                    panic!("plugin source must expose dispatch identity");
                };
                assert_ne!(identity, [0_u8; 32]);
                identity
            };

        let baseline = source_identity("fixture", [1; 32], json!({"mode": "one"}));
        let same = source_identity("fixture", [1; 32], json!({"mode": "one"}));
        assert_eq!(same, baseline);
        assert_ne!(
            source_identity("fixture-other", [1; 32], json!({"mode": "one"})),
            baseline
        );
        assert_ne!(
            source_identity("fixture", [2; 32], json!({"mode": "one"})),
            baseline
        );
        assert_ne!(
            source_identity("fixture", [1; 32], json!({"mode": "two"})),
            baseline
        );
    }

    #[test]
    fn runtime_semantic_fingerprint_is_generation_independent_and_base_section_sensitive() {
        let Some(next_generation) = ConfigGeneration::INITIAL.checked_next() else {
            panic!("test generation must have a successor");
        };

        let fixture = |catalog_marker: &str,
                       schema_marker: &str,
                       reverse_registration: bool,
                       first_dispatch_identity: [u8; 32],
                       permission_mode: Mode,
                       generation: ConfigGeneration| {
            let manifest = format!(
                r#"kind: AgentBundle
identity:
  id: hya/runtime-fingerprint
  version: 1.0.0
  publisher: hya-tests
agent:
  id: fingerprint
  description: "manifest {catalog_marker}"
  role: main
"#
            );
            let prepared = prepare_package(BundleSource::new(
                "runtime-fingerprint",
                vec![SourceFile::new("bundle.yaml", manifest.into_bytes())],
            ));
            let Ok(prepared) = prepared else {
                panic!("runtime fingerprint fixture preparation failed: {prepared:?}");
            };
            let catalog = TestCatalog::from_verified_catalogs(&[&prepared]);
            let Ok(catalog) = catalog else {
                panic!("runtime fingerprint verified catalog construction failed: {catalog:?}");
            };

            let registry = ToolRegistry::builtins();
            let mut custom_tools = vec![
                (
                    Arc::new(FingerprintTool::new("custom_one", schema_marker)) as Arc<dyn Tool>,
                    first_dispatch_identity,
                ),
                (
                    Arc::new(FingerprintTool::new("custom_two", "stable")) as Arc<dyn Tool>,
                    [2; 32],
                ),
            ];
            if reverse_registration {
                custom_tools.reverse();
            }
            for (tool, dispatch_identity) in custom_tools {
                if let Err(error) = registry
                    .register_with_permission_and_aliases_and_dispatch_identity(
                        tool,
                        ToolPermission::Tool,
                        &[],
                        dispatch_identity,
                    )
                {
                    panic!("runtime fingerprint tool fixture registration failed: {error}");
                }
            }

            let rules =
                PermissionRules::new(vec![Rule::new(Action::Tool, "custom_one", permission_mode)]);
            let policy = InvocationPolicy::compile(
                PermissionModel::Default,
                vec![InvocationRule::new(
                    PermissionTarget::Tool,
                    "^custom_one$",
                    Mode::Allow,
                )],
            );
            let Ok(policy) = policy else {
                panic!("runtime fingerprint invocation policy fixture must compile: {policy:?}");
            };
            let (permission, _asks) = PermissionPlane::new_with_policy(rules, policy);
            let tools = registry.snapshot();
            let snapshot = RuntimeSnapshot {
                generation,
                catalog: Arc::new(catalog),
                basic_tools: tools.clone(),
                tools,
                skills: BTreeMap::new(),
                sources: BTreeMap::new(),
                masks: Arc::new(BTreeMap::new()),
                schemes: Arc::new(BTreeMap::new()),
            };
            (
                TurnBinding {
                    snapshot: Arc::new(snapshot),
                    agent_model_preferences: Arc::new(BTreeMap::new()),
                    agent_model_configuration: Arc::new(AgentModelConfiguration::default()),
                    session_agent_models: Arc::new(BTreeMap::new()),
                    place: BindingPlace::new(
                        PathBuf::from("/tmp/runtime-fingerprint"),
                        CatalogScope::Global,
                    ),
                },
                permission,
            )
        };

        let fingerprint = |binding: &TurnBinding, permission: &PermissionPlane| {
            let Some(fingerprint) = binding.semantic_fingerprint_v1(permission) else {
                panic!("runtime semantic fingerprint should be available for this fixture");
            };
            assert_ne!(fingerprint, [0_u8; 32]);
            fingerprint
        };

        let (baseline_binding, baseline_permission) = fixture(
            "one",
            "one",
            false,
            [1; 32],
            Mode::Allow,
            ConfigGeneration::INITIAL,
        );
        let baseline = fingerprint(&baseline_binding, &baseline_permission);

        let (equivalent_binding, equivalent_permission) =
            fixture("one", "one", true, [1; 32], Mode::Allow, next_generation);
        assert_eq!(
            fingerprint(&equivalent_binding, &equivalent_permission),
            baseline,
            "fresh objects, registration order, and ConfigGeneration must not affect semantics"
        );

        let (catalog_binding, catalog_permission) = fixture(
            "two",
            "one",
            false,
            [1; 32],
            Mode::Allow,
            ConfigGeneration::INITIAL,
        );
        assert_ne!(
            fingerprint(&catalog_binding, &catalog_permission),
            baseline,
            "verified catalog semantics must affect the base fingerprint"
        );

        let (schema_binding, schema_permission) = fixture(
            "one",
            "two",
            false,
            [1; 32],
            Mode::Allow,
            ConfigGeneration::INITIAL,
        );
        assert_ne!(
            fingerprint(&schema_binding, &schema_permission),
            baseline,
            "tool schema semantics must affect the base fingerprint"
        );

        let (dispatch_binding, dispatch_permission) = fixture(
            "one",
            "one",
            false,
            [3; 32],
            Mode::Allow,
            ConfigGeneration::INITIAL,
        );
        assert_ne!(
            fingerprint(&dispatch_binding, &dispatch_permission),
            baseline,
            "explicit dispatch identity must affect the base fingerprint"
        );

        let (permission_binding, permission_variant) = fixture(
            "one",
            "one",
            false,
            [1; 32],
            Mode::Deny,
            ConfigGeneration::INITIAL,
        );
        assert_ne!(
            fingerprint(&permission_binding, &permission_variant),
            baseline,
            "permission-rule semantics must affect the base fingerprint"
        );
    }

    #[test]
    fn runtime_semantic_fingerprint_tracks_selected_workdir_skill_semantics() {
        let skill_fixture = |name: &str,
                             description: &str,
                             content: &str,
                             allowed_tools: &[&str],
                             model: Option<&str>,
                             path: &str| {
            SkillCatalogEntry {
                name: name.to_string(),
                description: description.to_string(),
                content: content.to_string(),
                allowed_tools: allowed_tools
                    .iter()
                    .map(|tool| (*tool).to_string())
                    .collect(),
                model: model.map(str::to_string),
                path: PathBuf::from(path),
                dir: PathBuf::from(path)
                    .parent()
                    .map_or_else(PathBuf::new, Path::to_path_buf),
                origin: hya_tool::SkillCatalogOrigin::Filesystem,
            }
        };
        let selected_skills = || {
            vec![
                skill_fixture(
                    "alpha",
                    "Alpha skill",
                    "alpha body",
                    &["custom_one"],
                    Some("model-a"),
                    "/tmp/runtime-fingerprint-skills/alpha/SKILL.md",
                ),
                skill_fixture(
                    "beta",
                    "Beta skill",
                    "beta body",
                    &[],
                    None,
                    "/tmp/runtime-fingerprint-skills/beta/SKILL.md",
                ),
            ]
        };
        let fixture = |selected: Vec<SkillCatalogEntry>, unrelated: Vec<SkillCatalogEntry>| {
            let manifest = r#"kind: AgentBundle
identity:
  id: hya/runtime-fingerprint-skills
  version: 1.0.0
  publisher: hya-tests
agent:
  id: fingerprint-skills
  role: main
"#;
            let prepared = prepare_package(BundleSource::new(
                "runtime-fingerprint-skills",
                vec![SourceFile::new("bundle.yaml", manifest.as_bytes())],
            ));
            let Ok(prepared) = prepared else {
                panic!("skill fingerprint fixture preparation failed: {prepared:?}");
            };
            let catalog = TestCatalog::from_verified_catalogs(&[&prepared]);
            let Ok(catalog) = catalog else {
                panic!("skill fingerprint verified catalog construction failed: {catalog:?}");
            };
            let tools = ToolRegistry::builtins().snapshot();
            let workdir = PathBuf::from("/tmp/runtime-fingerprint-skills");
            let mut skills = BTreeMap::new();
            skills.insert(workdir.clone(), Arc::new(selected));
            if !unrelated.is_empty() {
                skills.insert(
                    PathBuf::from("/tmp/runtime-fingerprint-unrelated"),
                    Arc::new(unrelated),
                );
            }
            let snapshot = RuntimeSnapshot {
                generation: ConfigGeneration::INITIAL,
                catalog: Arc::new(catalog),
                basic_tools: tools.clone(),
                tools,
                skills,
                sources: BTreeMap::new(),
                masks: Arc::new(BTreeMap::new()),
                schemes: Arc::new(BTreeMap::new()),
            };
            let (permission, _asks) = PermissionPlane::new_with_policy(
                PermissionRules::default(),
                InvocationPolicy::default(),
            );
            (
                TurnBinding {
                    snapshot: Arc::new(snapshot),
                    agent_model_preferences: Arc::new(BTreeMap::new()),
                    agent_model_configuration: Arc::new(AgentModelConfiguration::default()),
                    session_agent_models: Arc::new(BTreeMap::new()),
                    place: BindingPlace::new(workdir, CatalogScope::Global),
                },
                permission,
            )
        };
        let fingerprint = |binding: &TurnBinding, permission: &PermissionPlane| {
            let Some(fingerprint) = binding.semantic_fingerprint_v1(permission) else {
                panic!("selected workdir skills must be fingerprintable");
            };
            assert_ne!(fingerprint, [0_u8; 32]);
            fingerprint
        };

        let (baseline_binding, baseline_permission) = fixture(selected_skills(), Vec::new());
        let baseline = fingerprint(&baseline_binding, &baseline_permission);
        let (equivalent_binding, equivalent_permission) = fixture(selected_skills(), Vec::new());
        assert_eq!(
            fingerprint(&equivalent_binding, &equivalent_permission),
            baseline,
            "fresh skill entries, catalogs, and permission planes must match"
        );

        let mut changed_content = selected_skills();
        changed_content[0].content = "changed body".to_string();
        let (content_binding, content_permission) = fixture(changed_content, Vec::new());
        assert_ne!(
            fingerprint(&content_binding, &content_permission),
            baseline,
            "skill content must affect the fingerprint"
        );

        let mut renamed = selected_skills();
        renamed[0].name = "alpha-renamed".to_string();
        let (rename_binding, rename_permission) = fixture(renamed, Vec::new());
        assert_ne!(
            fingerprint(&rename_binding, &rename_permission),
            baseline,
            "skill identity must affect the fingerprint"
        );

        let mut moved = selected_skills();
        moved[0].path = PathBuf::from("/tmp/runtime-fingerprint-skills/moved/SKILL.md");
        moved[0].dir = PathBuf::from("/tmp/runtime-fingerprint-skills/moved");
        let (path_binding, path_permission) = fixture(moved, Vec::new());
        assert_ne!(
            fingerprint(&path_binding, &path_permission),
            baseline,
            "skill path must affect the fingerprint"
        );

        let mut changed_description = selected_skills();
        changed_description[0].description = "changed description".to_string();
        let (description_binding, description_permission) =
            fixture(changed_description, Vec::new());
        assert_ne!(
            fingerprint(&description_binding, &description_permission),
            baseline,
            "skill semantic metadata must affect the fingerprint"
        );

        let mut changed_allowed_tools = selected_skills();
        changed_allowed_tools[0].allowed_tools = vec!["custom_two".to_string()];
        let (allowed_tools_binding, allowed_tools_permission) =
            fixture(changed_allowed_tools, Vec::new());
        assert_ne!(
            fingerprint(&allowed_tools_binding, &allowed_tools_permission),
            baseline,
            "skill semantic metadata must affect the fingerprint"
        );

        let mut changed_model = selected_skills();
        changed_model[0].model = Some("model-b".to_string());
        let (model_binding, model_permission) = fixture(changed_model, Vec::new());
        assert_ne!(
            fingerprint(&model_binding, &model_permission),
            baseline,
            "skill semantic metadata must affect the fingerprint"
        );

        let mut reversed = selected_skills();
        reversed.reverse();
        let (reversed_binding, reversed_permission) = fixture(reversed, Vec::new());
        assert_ne!(
            fingerprint(&reversed_binding, &reversed_permission),
            baseline,
            "selected skill order must preserve precedence semantics"
        );

        let unrelated = vec![skill_fixture(
            "unrelated",
            "unrelated skill",
            "unrelated body",
            &[],
            None,
            "/tmp/runtime-fingerprint-unrelated/unrelated/SKILL.md",
        )];
        let (unrelated_binding, unrelated_permission) = fixture(selected_skills(), unrelated);
        assert_eq!(
            fingerprint(&unrelated_binding, &unrelated_permission),
            baseline,
            "skills cached for another workdir must not affect this binding"
        );
    }

    #[test]
    fn runtime_semantic_fingerprint_tracks_plugin_and_mcp_source_semantics() {
        let nested_value = |marker: &str, reverse: bool| {
            let mut nested = serde_json::Map::new();
            let entries = [
                ("marker", Value::String(marker.to_string())),
                ("enabled", Value::Bool(true)),
            ];
            if reverse {
                for (key, value) in entries.into_iter().rev() {
                    nested.insert(key.to_string(), value);
                }
            } else {
                for (key, value) in entries {
                    nested.insert(key.to_string(), value);
                }
            }
            let mut outer = serde_json::Map::new();
            if reverse {
                outer.insert("nested".to_string(), Value::Object(nested));
                outer.insert("version".to_string(), Value::from(1));
            } else {
                outer.insert("version".to_string(), Value::from(1));
                outer.insert("nested".to_string(), Value::Object(nested));
            }
            Value::Object(outer)
        };
        let fixture = |plugin_kind: RuntimeSourceKind,
                       plugin_id: &str,
                       plugin_digest: [u8; 32],
                       plugin_resource_marker: &str,
                       mcp_digest: [u8; 32],
                       mcp_resource_marker: &str,
                       include_mcp: bool,
                       reverse_sources: bool,
                       reverse_resources: bool| {
            let manifest = r#"kind: AgentBundle
identity:
  id: hya/runtime-fingerprint-sources
  version: 1.0.0
  publisher: hya-tests
agent:
  id: fingerprint-sources
  role: main
"#;
            let prepared = prepare_package(BundleSource::new(
                "runtime-fingerprint-sources",
                vec![SourceFile::new("bundle.yaml", manifest.as_bytes())],
            ));
            let Ok(prepared) = prepared else {
                panic!("source fingerprint fixture preparation failed: {prepared:?}");
            };
            let catalog = TestCatalog::from_verified_catalogs(&[&prepared]);
            let Ok(catalog) = catalog else {
                panic!("source fingerprint verified catalog construction failed: {catalog:?}");
            };
            let registry = test_runtime_registry(ToolRegistry::builtins(), Arc::new(catalog));
            let mut plugin_resources = BTreeMap::new();
            plugin_resources.insert(
                "config".to_string(),
                nested_value(plugin_resource_marker, reverse_resources),
            );
            let probe_name = match plugin_kind {
                RuntimeSourceKind::Mcp => format!("mcp__{plugin_id}__probe"),
                _ => format!("plugin__{plugin_id}__probe"),
            };
            let plugin = RuntimeSource::new(
                RuntimeSourceId::new(plugin_kind, plugin_id),
                plugin_digest,
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "probe",
                    probe_name.clone(),
                    vec!["plugin_probe".to_string()],
                    Arc::new(NoopTool::new(&probe_name)),
                    ToolPermission::Tool,
                )],
            )
            .with_resources(plugin_resources);
            let mut mcp_resources = BTreeMap::new();
            mcp_resources.insert(
                "config".to_string(),
                nested_value(mcp_resource_marker, reverse_resources),
            );
            let mcp = RuntimeSource::new(
                RuntimeSourceId::mcp("fixture-mcp"),
                mcp_digest,
                Arc::new(()),
                Vec::new(),
            )
            .with_resources(mcp_resources);
            let mut sources = vec![plugin];
            if include_mcp {
                sources.push(mcp);
            }
            if reverse_sources {
                sources.reverse();
            }
            let refreshed = registry.refresh(|candidate| candidate.upsert_sources(sources));
            let Ok(_) = refreshed else {
                panic!("source fingerprint refresh failed: {refreshed:?}");
            };
            let tools = registry.active();
            let (permission, _asks) = PermissionPlane::new_with_policy(
                PermissionRules::default(),
                InvocationPolicy::default(),
            );
            (
                TurnBinding {
                    agent_model_configuration: Arc::new(AgentModelConfiguration::default()),
                    session_agent_models: Arc::new(BTreeMap::new()),
                    snapshot: tools,
                    agent_model_preferences: Arc::new(BTreeMap::new()),
                    place: BindingPlace::new(
                        PathBuf::from("/tmp/runtime-fingerprint-sources"),
                        CatalogScope::Global,
                    ),
                },
                permission,
            )
        };
        let fingerprint = |binding: &TurnBinding, permission: &PermissionPlane| {
            let Some(fingerprint) = binding.semantic_fingerprint_v1(permission) else {
                panic!("runtime source semantics must be fingerprintable");
            };
            assert_ne!(fingerprint, [0_u8; 32]);
            fingerprint
        };

        let (baseline_binding, baseline_permission) = fixture(
            RuntimeSourceKind::Plugin,
            "fixture-plugin",
            [1; 32],
            "plugin-one",
            [2; 32],
            "mcp-one",
            true,
            false,
            false,
        );
        let baseline = fingerprint(&baseline_binding, &baseline_permission);
        let (equivalent_binding, equivalent_permission) = fixture(
            RuntimeSourceKind::Plugin,
            "fixture-plugin",
            [1; 32],
            "plugin-one",
            [2; 32],
            "mcp-one",
            true,
            true,
            true,
        );
        assert_eq!(
            fingerprint(&equivalent_binding, &equivalent_permission),
            baseline,
            "fresh owners, source order, and nested JSON order must not affect semantics"
        );

        let (source_kind_binding, source_kind_permission) = fixture(
            RuntimeSourceKind::Mcp,
            "fixture-plugin",
            [1; 32],
            "plugin-one",
            [2; 32],
            "mcp-one",
            true,
            false,
            false,
        );
        assert_ne!(
            fingerprint(&source_kind_binding, &source_kind_permission),
            baseline,
            "runtime source kind must affect the fingerprint"
        );

        let (plugin_id_binding, plugin_id_permission) = fixture(
            RuntimeSourceKind::Plugin,
            "fixture-plugin-other",
            [1; 32],
            "plugin-one",
            [2; 32],
            "mcp-one",
            true,
            false,
            false,
        );
        assert_ne!(
            fingerprint(&plugin_id_binding, &plugin_id_permission),
            baseline,
            "plugin configured ID must affect the fingerprint"
        );

        let (plugin_digest_binding, plugin_digest_permission) = fixture(
            RuntimeSourceKind::Plugin,
            "fixture-plugin",
            [3; 32],
            "plugin-one",
            [2; 32],
            "mcp-one",
            true,
            false,
            false,
        );
        assert_ne!(
            fingerprint(&plugin_digest_binding, &plugin_digest_permission),
            baseline,
            "plugin declaration digest must affect the fingerprint"
        );

        let (plugin_resource_binding, plugin_resource_permission) = fixture(
            RuntimeSourceKind::Plugin,
            "fixture-plugin",
            [1; 32],
            "plugin-two",
            [2; 32],
            "mcp-one",
            true,
            false,
            false,
        );
        assert_ne!(
            fingerprint(&plugin_resource_binding, &plugin_resource_permission),
            baseline,
            "plugin resource semantics must affect the fingerprint"
        );

        let (mcp_digest_binding, mcp_digest_permission) = fixture(
            RuntimeSourceKind::Plugin,
            "fixture-plugin",
            [1; 32],
            "plugin-one",
            [3; 32],
            "mcp-one",
            true,
            false,
            false,
        );
        assert_ne!(
            fingerprint(&mcp_digest_binding, &mcp_digest_permission),
            baseline,
            "zero-export MCP declaration digest must affect the fingerprint"
        );

        let (mcp_resource_binding, mcp_resource_permission) = fixture(
            RuntimeSourceKind::Plugin,
            "fixture-plugin",
            [1; 32],
            "plugin-one",
            [2; 32],
            "mcp-two",
            true,
            false,
            false,
        );
        assert_ne!(
            fingerprint(&mcp_resource_binding, &mcp_resource_permission),
            baseline,
            "zero-export MCP resources must affect the fingerprint"
        );

        let (without_mcp_binding, without_mcp_permission) = fixture(
            RuntimeSourceKind::Plugin,
            "fixture-plugin",
            [1; 32],
            "plugin-one",
            [2; 32],
            "mcp-one",
            false,
            false,
            false,
        );
        assert_ne!(
            fingerprint(&without_mcp_binding, &without_mcp_permission),
            baseline,
            "removing a zero-export MCP source must affect the fingerprint"
        );
    }

    #[test]
    fn missing_and_filtered_alias_targets_fail_typed() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-miss",
                agent(
                    "alias-miss",
                    ResourceView {
                        allow: Vec::new(),
                        deny: Vec::new(),
                        aliases: BTreeMap::from([(
                            "marker".to_string(),
                            "harness:tool/dynamic_marker".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        // Register after construction so basic_tools stays builtins-only.
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("dynamic_marker"))))
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-miss"))
            .unwrap();
        let policy = binding.agent_resource_policy("alias-miss").unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::UnknownResourceReference { .. }) => {}
            Ok(_) => panic!("expected unknown filtered alias target"),
            Err(other) => panic!("expected unknown filtered alias target, got {other:?}"),
        }
    }

    #[test]
    fn deny_filtered_alias_target_fails_typed() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-denied",
                agent(
                    "alias-denied",
                    ResourceView {
                        allow: Vec::new(),
                        deny: vec!["harness:tool/read".to_string()],
                        aliases: BTreeMap::from([(
                            "reader".to_string(),
                            "harness:tool/read".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-denied"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("alias-denied", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::UnknownResourceReference { .. }) => {}
            Ok(_) => panic!("expected denied alias target to fail"),
            Err(other) => panic!("expected denied alias target to fail, got {other:?}"),
        }
    }

    #[test]
    fn alias_cannot_override_qualified_stable_name() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-qualified",
                agent(
                    "alias-qualified",
                    ResourceView {
                        allow: Vec::new(),
                        deny: Vec::new(),
                        aliases: BTreeMap::from([(
                            "harness:tool/read".to_string(),
                            "harness:tool/write".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-qualified"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("alias-qualified", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::AliasCollision { name, .. }) => {
                assert_eq!(name, "harness:tool/read");
            }
            Ok(_) => panic!("expected qualified alias override to fail"),
            Err(other) => panic!("expected alias collision, got {other:?}"),
        }
    }

    #[test]
    fn alias_collision_with_public_short_name_fails_typed() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-collide",
                agent(
                    "alias-collide",
                    ResourceView {
                        allow: Vec::new(),
                        deny: Vec::new(),
                        aliases: BTreeMap::from([(
                            "read".to_string(),
                            "harness:tool/write".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-collide"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("alias-collide", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::AliasCollision { bundle_id, name }) => {
                assert_eq!(bundle_id, "hya/alias-collide");
                assert_eq!(name, "read");
            }
            Ok(_) => panic!("expected alias collision"),
            Err(other) => panic!("expected alias collision, got {other:?}"),
        }
    }

    #[test]
    fn local_skill_short_name_wins_and_filtered_local_restores_harness_short() {
        let local = PreparedResource {
            local_id: "shared".to_string(),
            stable_id: "bundle:hya/skill-collision/skill/shared".to_string(),
            source_path: "resources/skills/shared.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("shared", "LOCAL"),
            binary_base64: None,
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/skill-collision",
                agent("skill-agent", ResourceView::default()),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let workdir = tempfile_skill_workdir("shared", "HARNESS");
        let binding = registry.bind_turn(&workdir).unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("skill-agent", AgentToolPlane::Full)
            .unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        let names = compiled
            .skills()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<BTreeSet<_>>();
        assert!(names.contains("shared"));
        assert!(names.contains("harness:skill/shared"));
        let shared = compiled
            .skills()
            .iter()
            .find(|skill| skill.name == "shared")
            .unwrap();
        assert!(shared.content.contains("LOCAL"));

        let filtered_catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/skill-collision",
                agent(
                    "skill-agent",
                    ResourceView {
                        allow: vec![
                            "harness:skill/shared".to_string(),
                            "harness:tool/skill".to_string(),
                        ],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                vec![PreparedResource {
                    local_id: "shared".to_string(),
                    stable_id: "bundle:hya/skill-collision/skill/shared".to_string(),
                    source_path: "resources/skills/shared.md".to_string(),
                    digest: "test-only".to_string(),
                    content: skill_md("shared", "LOCAL"),
                    binary_base64: None,
                    aliases: Vec::new(),
                }],
            )])
            .unwrap(),
        );
        let filtered_registry = test_runtime_registry(ToolRegistry::builtins(), filtered_catalog);
        let filtered_binding = filtered_registry.bind_turn(&workdir).unwrap();
        let filtered_policy = filtered_binding
            .agent_resource_policy_on_plane("skill-agent", AgentToolPlane::Full)
            .unwrap();
        let filtered = filtered_binding
            .compile_agent_resources(&filtered_policy)
            .unwrap();
        let filtered_names = filtered
            .skills()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            filtered_names,
            BTreeSet::from(["shared", "harness:skill/shared"]),
            "filtering the local restores harness short and keeps qualified spelling"
        );
        assert!(
            filtered
                .skills()
                .iter()
                .any(|skill| skill.name == "shared" && skill.content.contains("HARNESS"))
        );
    }

    #[test]
    fn model_schemas_use_explicit_aliases_and_canonical_short_names() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/schema-dispatch",
                agent(
                    "sd-agent",
                    ResourceView {
                        allow: vec![
                            "harness:tool/read".to_string(),
                            "harness:tool/write".to_string(),
                        ],
                        deny: Vec::new(),
                        aliases: BTreeMap::from([(
                            "reader".to_string(),
                            "harness:tool/read".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-schema-dispatch"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("sd-agent", AgentToolPlane::Full)
            .unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        let schema_names = domain_schema_names(&compiled);
        assert_eq!(
            schema_names,
            BTreeSet::from(["reader".to_string(), "write".to_string()])
        );
        assert!(compiled.resolve_tool("reader").is_some());
        assert!(compiled.resolve_tool("read").is_none());
        assert!(compiled.resolve_tool("write").is_some());
        assert!(
            compiled.resolve_tool("harness:tool/read").is_some(),
            "alias must preserve the exact qualified binding"
        );
        assert!(compiled.resolve_tool("harness:tool/write").is_some());
    }

    #[test]
    fn a_bundle_local_allow_list_yields_no_skill_facade_and_inlines_the_skill() {
        let local = PreparedResource {
            local_id: "bundle-skill".to_string(),
            stable_id: "bundle:hya/none-inline/skill/bundle-skill".to_string(),
            source_path: "resources/skills/bundle-skill.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("bundle-skill", "INLINE_BODY_MARKER"),
            binary_base64: None,
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/none-inline",
                agent(
                    "none-agent",
                    ResourceView {
                        allow: vec!["bundle:hya/none-inline/skill/bundle-skill".to_string()],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("dynamic_marker"))))
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-none-inline"))
            .unwrap();
        let policy = binding.agent_resource_policy("none-agent").unwrap();
        assert_eq!(
            policy.selected_bundle_skill_ids(),
            &["bundle:hya/none-inline/skill/bundle-skill".to_string()]
        );
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        assert!(
            domain_tool_names(&compiled).is_empty(),
            "an allow list of only bundle-local resources admits no harness domain tool"
        );
        assert!(
            compiled.resolve_tool("skill").is_none(),
            "no harness skill tool is inserted when none is selected"
        );
        assert!(
            compiled.resolve_tool("dynamic_marker").is_none(),
            "dispatch must not fall back to the live registry"
        );
        assert!(
            compiled.resolve_tool("bash").is_none(),
            "dispatch must not fall back to builtins outside the compiled view"
        );
        assert!(
            has_mail_only_read(&compiled),
            "without its own read the view gets only the mail-only channel read"
        );
        let section = compiled
            .skills_prompt_section()
            .expect("selected local static skill must produce prompt content");
        assert!(
            section.contains("INLINE_BODY_MARKER"),
            "local static skill body must be inlined when no skill facade exists: {section}"
        );
        assert!(
            section.contains("bundle-skill"),
            "local static skill name must remain consumable via prompt content"
        );
        let unique_paths = compiled
            .skills()
            .iter()
            .map(|skill| skill.path.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            unique_paths.len(),
            1,
            "short and qualified public spellings share one selected local skill"
        );
        assert!(
            compiled
                .skills()
                .iter()
                .any(|skill| skill.name == "bundle-skill")
        );
        assert!(
            compiled
                .skills()
                .iter()
                .any(|skill| { skill.name == "bundle:hya/none-inline/skill/bundle-skill" }),
            "qualified public spelling must be real for selected local skills"
        );
    }

    #[test]
    fn qualified_public_spellings_namespace_alias_and_dispatch() {
        let local = PreparedResource {
            local_id: "probe".to_string(),
            stable_id: "bundle:hya/qualified-map/skill/probe".to_string(),
            source_path: "resources/skills/probe.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("probe", "LOCAL_PROBE"),
            binary_base64: None,
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/qualified-map",
                agent(
                    "q-agent",
                    ResourceView {
                        allow: vec![
                            "harness:tool/read".to_string(),
                            "harness:tool/write".to_string(),
                            "bundle:hya/qualified-map/skill/probe".to_string(),
                        ],
                        deny: Vec::new(),
                        aliases: BTreeMap::from([
                            ("reader".to_string(), "harness:tool/read".to_string()),
                            ("book".to_string(), "harness:tool/read".to_string()),
                        ]),
                        namespace: Some("custom.ns".to_string()),
                    },
                ),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-qualified-map"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("q-agent", AgentToolPlane::Full)
            .unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        let schema_names = domain_schema_names(&compiled);
        assert_eq!(
            schema_names,
            BTreeSet::from([
                "book".to_string(),
                "reader".to_string(),
                "write".to_string()
            ])
        );
        assert!(compiled.resolve_tool("reader").is_some());
        assert!(compiled.resolve_tool("book").is_some());
        assert!(
            compiled.resolve_tool("read").is_none(),
            "alias replaces the ordinary short spelling"
        );
        assert!(
            compiled.resolve_tool("harness:tool/read").is_some(),
            "exact qualified binding must remain after aliases"
        );
        assert!(compiled.resolve_tool("write").is_some());
        assert!(compiled.resolve_tool("harness:tool/write").is_some());
        let skill_names = compiled
            .skills()
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<BTreeSet<_>>();
        assert!(
            skill_names.contains("probe"),
            "bundle-local short remains available: {skill_names:?}"
        );
        assert!(
            skill_names.contains("bundle:custom.ns/skill/probe"),
            "namespace changes only the bundle-local qualified public spelling: {skill_names:?}"
        );
        assert!(
            !skill_names.contains("bundle:hya/qualified-map/skill/probe")
                || skill_names.contains("bundle:custom.ns/skill/probe"),
            "custom namespace must be the public qualified spelling"
        );
    }

    #[test]
    fn harness_mcp_is_source_owned_full_only_and_pins_with_binding() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[
                bundle_with_agent(
                    "hya/mcp-full",
                    agent("full-mcp", ResourceView::default()),
                    Vec::new(),
                ),
                bundle_with_agent(
                    "hya/mcp-basic",
                    agent("basic-mcp", ResourceView::default()),
                    Vec::new(),
                ),
                bundle_with_agent(
                    "hya/mcp-allow",
                    agent(
                        "allow-mcp",
                        ResourceView {
                            allow: vec!["harness:mcp/mcp__fixture__ping".to_string()],
                            deny: Vec::new(),
                            aliases: BTreeMap::new(),
                            namespace: None,
                        },
                    ),
                    Vec::new(),
                ),
                bundle_with_agent(
                    "hya/mcp-deny",
                    agent(
                        "deny-mcp",
                        ResourceView {
                            allow: Vec::new(),
                            deny: vec!["harness:mcp/mcp__fixture__ping".to_string()],
                            aliases: BTreeMap::new(),
                            namespace: None,
                        },
                    ),
                    Vec::new(),
                ),
            ])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![RuntimeSource::new(
                    RuntimeSourceId::mcp("fixture"),
                    [7; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "ping",
                        "mcp__fixture__ping",
                        Vec::new(),
                        Arc::new(NoopTool::new("mcp__fixture__ping")),
                        ToolPermission::Mcp,
                    )],
                )])
            })
            .unwrap();
        let workdir = Path::new("/tmp/hya-mcp-kind");
        let binding = registry.bind_turn(workdir).unwrap();

        let full = binding
            .compile_agent_resources(
                &binding
                    .agent_resource_policy_on_plane("full-mcp", AgentToolPlane::Full)
                    .unwrap(),
            )
            .unwrap();
        assert!(full.resolve_tool("mcp__fixture__ping").is_some());
        assert!(
            full.resolve_tool("harness:mcp/mcp__fixture__ping")
                .is_some()
        );
        assert!(
            full.resolve_tool("harness:tool/mcp__fixture__ping")
                .is_none(),
            "MCP must not be re-homed under the tool kind"
        );

        let basic = binding
            .compile_agent_resources(&binding.agent_resource_policy("basic-mcp").unwrap())
            .unwrap();
        assert!(basic.resolve_tool("mcp__fixture__ping").is_none());
        assert!(
            basic
                .resolve_tool("harness:mcp/mcp__fixture__ping")
                .is_none()
        );

        let allowed = binding
            .compile_agent_resources(
                &binding
                    .agent_resource_policy_on_plane("allow-mcp", AgentToolPlane::Full)
                    .unwrap(),
            )
            .unwrap();
        assert!(allowed.resolve_tool("mcp__fixture__ping").is_some());
        assert!(!domain_tool_names(&allowed).contains("read"));

        let denied = binding
            .compile_agent_resources(
                &binding
                    .agent_resource_policy_on_plane("deny-mcp", AgentToolPlane::Full)
                    .unwrap(),
            )
            .unwrap();
        assert!(denied.resolve_tool("mcp__fixture__ping").is_none());
        assert!(denied.resolve_tool("read").is_some());

        // Pin: remove MCP source after binding capture.
        registry
            .refresh(|candidate| {
                let mut removed = BTreeSet::new();
                removed.insert(RuntimeSourceId::mcp("fixture"));
                candidate.remove_sources(&removed);
                Ok(())
            })
            .unwrap();
        let pinned = binding
            .compile_agent_resources(
                &binding
                    .agent_resource_policy_on_plane("full-mcp", AgentToolPlane::Full)
                    .unwrap(),
            )
            .unwrap();
        assert!(
            pinned.resolve_tool("mcp__fixture__ping").is_some(),
            "old TurnBinding must pin the prior MCP view"
        );
        let fresh = registry.bind_turn(workdir).unwrap();
        let after = fresh
            .compile_agent_resources(
                &fresh
                    .agent_resource_policy_on_plane("full-mcp", AgentToolPlane::Full)
                    .unwrap(),
            )
            .unwrap();
        assert!(after.resolve_tool("mcp__fixture__ping").is_none());
    }

    #[test]
    fn global_canonical_reference_rejects_wrong_kind_and_cross_kind_short_ambiguity() {
        let local = PreparedResource {
            local_id: "shared".to_string(),
            stable_id: "bundle:hya/global-ref/skill/shared".to_string(),
            source_path: "resources/skills/shared.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("shared", "LOCAL"),
            binary_base64: None,
            aliases: Vec::new(),
        };
        let wrong_kind = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/global-ref",
                agent(
                    "wrong-kind",
                    ResourceView {
                        allow: vec!["harness:skill/read".to_string()],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), wrong_kind);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-global-wrong-kind"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("wrong-kind", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::UnknownResourceReference { reference, .. }) => {
                assert_eq!(reference, "harness:skill/read");
            }
            Ok(_) => panic!("wrong-kind prefix must typed-reject"),
            Err(other) => panic!("expected unknown resource reference, got {other:?}"),
        }

        let ambiguous = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/global-ref",
                agent(
                    "ambiguous",
                    ResourceView {
                        allow: vec!["shared".to_string()],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), ambiguous);
        // Register a harness tool whose short name collides with the local skill.
        registry
            .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("shared"))))
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-global-ambiguous"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("ambiguous", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::NamespaceCollision { name, .. }) => {
                assert_eq!(name, "shared");
            }
            Ok(_) => panic!("cross-kind short ambiguity must typed-reject"),
            Err(other) => panic!("expected namespace collision, got {other:?}"),
        }
    }

    #[test]
    fn alias_cannot_occupy_same_target_stable_spelling() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-self",
                agent(
                    "alias-self",
                    ResourceView {
                        allow: Vec::new(),
                        deny: Vec::new(),
                        aliases: BTreeMap::from([(
                            "read".to_string(),
                            "harness:tool/read".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-self"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("alias-self", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::AliasCollision { name, .. }) => {
                assert_eq!(name, "read");
            }
            Ok(_) => panic!("alias occupying the target short spelling must fail"),
            Err(other) => panic!("expected alias collision, got {other:?}"),
        }
    }

    #[test]
    fn tool_and_skill_may_share_public_spelling_independently() {
        let local = PreparedResource {
            local_id: "read".to_string(),
            stable_id: "bundle:hya/cross-kind-ok/skill/read".to_string(),
            source_path: "resources/skills/read.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("read", "SKILL_READ_BODY"),
            binary_base64: None,
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/cross-kind-ok",
                agent(
                    "cross-ok",
                    ResourceView {
                        allow: vec![
                            "harness:tool/read".to_string(),
                            "bundle:hya/cross-kind-ok/skill/read".to_string(),
                        ],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-cross-kind-ok"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("cross-ok", AgentToolPlane::Full)
            .unwrap();
        let compiled = binding
            .compile_agent_resources(&policy)
            .expect("tool and skill may share public spelling `read`");
        let tool = compiled
            .resolve_tool("read")
            .expect("tool public name `read` must dispatch");
        assert_eq!(tool.tool.name(), "read");
        assert!(
            compiled
                .skills()
                .iter()
                .any(|skill| skill.name == "read" && skill.content.contains("SKILL_READ_BODY")),
            "skill public name `read` must remain independently addressable"
        );
    }

    #[test]
    fn tool_mcp_public_name_collision_is_typed_rejected() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/tool-mcp-collide",
                agent(
                    "collide",
                    ResourceView {
                        allow: vec![
                            "harness:tool/read".to_string(),
                            "harness:mcp/mcp__fixture__ping".to_string(),
                        ],
                        deny: Vec::new(),
                        // Alias the tool onto the MCP public short spelling.
                        aliases: BTreeMap::from([(
                            "mcp__fixture__ping".to_string(),
                            "harness:tool/read".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![RuntimeSource::new(
                    RuntimeSourceId::mcp("fixture"),
                    [1; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "ping",
                        "mcp__fixture__ping",
                        Vec::new(),
                        Arc::new(NoopTool::new("mcp__fixture__ping")),
                        ToolPermission::Mcp,
                    )],
                )])
            })
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-tool-mcp-collide"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("collide", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::NamespaceCollision { name, .. })
            | Err(BundleError::AliasCollision { name, .. }) => {
                assert_eq!(name, "mcp__fixture__ping");
            }
            Ok(_) => panic!("tool/MCP shared dispatch collision must typed-reject"),
            Err(other) => panic!("expected tool/MCP collision, got {other:?}"),
        }
    }

    #[test]
    fn cross_kind_ambiguous_alias_target_fails_typed() {
        let local = PreparedResource {
            local_id: "shared".to_string(),
            stable_id: "bundle:hya/alias-ambiguous/skill/shared".to_string(),
            source_path: "resources/skills/shared.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("shared", "LOCAL"),
            binary_base64: None,
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-ambiguous",
                agent(
                    "alias-amb",
                    ResourceView {
                        allow: Vec::new(),
                        deny: Vec::new(),
                        aliases: BTreeMap::from([("marker".to_string(), "shared".to_string())]),
                        namespace: None,
                    },
                ),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("shared"))))
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-ambiguous"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("alias-amb", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::NamespaceCollision { name, .. }) => {
                assert_eq!(name, "shared");
            }
            Ok(_) => panic!("cross-kind ambiguous alias target must typed-reject"),
            Err(other) => panic!("expected namespace collision, got {other:?}"),
        }
    }

    #[test]
    fn both_planes_keep_builtin_aliases_out_of_model_schemas() {
        let expected = ToolRegistry::builtins()
            .schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(expected.len(), 28);
        for (agent_id, plane, workdir) in [
            (
                "full-alias",
                AgentToolPlane::Full,
                "/tmp/hya-builtin-alias-full",
            ),
            (
                "internal-public-alias",
                AgentToolPlane::InternalPublic,
                "/tmp/hya-builtin-alias-internal-public",
            ),
        ] {
            let catalog = Arc::new(
                TestCatalog::from_prepared(&[bundle_with_agent(
                    "hya/builtin-alias",
                    agent(agent_id, ResourceView::default()),
                    Vec::new(),
                )])
                .unwrap(),
            );
            let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
            let binding = registry.bind_turn(Path::new(workdir)).unwrap();
            let policy = binding
                .agent_resource_policy_on_plane(agent_id, plane)
                .unwrap();
            let compiled = binding.compile_agent_resources(&policy).unwrap();
            assert!(
                compiled.resolve_tool("apply_patch").is_some(),
                "{agent_id}: canonical apply_patch must remain public"
            );
            for alias in ["fetch", "search", "patch", "plan", "shell", "question"] {
                assert!(
                    compiled.resolve_tool(alias).is_some(),
                    "{agent_id}: hidden alias `{alias}` must remain dispatchable"
                );
            }
            let schema_names = compiled
                .tool_schemas()
                .into_iter()
                .map(|schema| schema.name.as_str().to_string())
                .collect::<BTreeSet<_>>();
            // A bundle agent that can spawn nobody gets no `task`/`archive`.
            let mut expected = expected.clone();
            for name in crate::coordination::SPAWN_TOOLS {
                expected.remove(name);
            }
            assert_eq!(
                schema_names, expected,
                "{agent_id}: canonical schema set drifted"
            );
            assert!(
                schema_names.contains("apply_patch"),
                "{agent_id}: canonical schema must remain model-facing"
            );
            assert!(
                !schema_names.contains("patch") && !schema_names.contains("shell"),
                "{agent_id}: hidden dispatch aliases must not be advertised"
            );
            assert!(
                schema_names.iter().all(|name| !name.contains([':', '/'])),
                "{agent_id}: qualified dispatch identities must not reach providers: {schema_names:?}"
            );
        }
    }

    #[test]
    fn explicit_view_alias_suppresses_candidate_aliases_for_target() {
        // Consult23: after ResourceView explicitly aliases a selected canonical
        // target, only the explicit public aliases and the qualified stable name
        // remain callable. Candidate registry aliases (e.g. `patch`) must not
        // re-enter the public map and bypass the mapping.
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/view-alias-suppress",
                agent(
                    "suppress-agent",
                    ResourceView {
                        allow: Vec::new(),
                        deny: Vec::new(),
                        aliases: BTreeMap::from([(
                            "applier".to_string(),
                            "harness:tool/apply_patch".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-view-alias-suppress"))
            .unwrap();
        let compiled = binding
            .compile_agent_resources(
                &binding
                    .agent_resource_policy_on_plane("suppress-agent", AgentToolPlane::Full)
                    .unwrap(),
            )
            .expect("explicit view alias of apply_patch must compile");

        assert!(
            compiled.resolve_tool("applier").is_some(),
            "explicit view alias `applier` must be callable"
        );
        assert!(
            compiled.resolve_tool("harness:tool/apply_patch").is_some(),
            "qualified stable name must remain callable"
        );
        assert!(
            compiled.resolve_tool("apply_patch").is_none(),
            "ordinary short spelling must be removed after explicit aliasing"
        );
        assert!(
            compiled.resolve_tool("patch").is_none(),
            "preexisting candidate alias `patch` must not re-enter after explicit aliasing"
        );

        let schema_names = compiled
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<BTreeSet<_>>();
        assert!(schema_names.is_subset(&compiled.public_tool_names()));
        assert!(schema_names.contains("applier"));
        assert!(!schema_names.contains("harness:tool/apply_patch"));
        assert!(!schema_names.contains("apply_patch"));
        assert!(!schema_names.contains("patch"));
        assert!(
            schema_names
                .iter()
                .all(|name| !name.contains(':') && !name.contains('/'))
        );
    }

    #[test]
    fn invalid_provider_tool_alias_fails_during_resource_compile() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/invalid-provider-alias",
                agent(
                    "invalid-alias-agent",
                    ResourceView {
                        allow: vec!["harness:tool/read".to_string()],
                        deny: Vec::new(),
                        aliases: BTreeMap::from([(
                            "bad:name".to_string(),
                            "harness:tool/read".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-invalid-provider-alias"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("invalid-alias-agent", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::InvalidManifest { detail, .. }) => {
                assert!(detail.contains("model-facing tool name `bad:name`"));
            }
            Ok(_) => panic!("invalid provider Tool alias must fail before a request"),
            Err(other) => panic!("expected invalid manifest, got {other:?}"),
        }
    }

    #[test]
    fn explicit_view_alias_collides_with_candidate_alias_even_same_target() {
        // Explicit view alias named `patch` targeting harness:tool/apply_patch
        // collides with the existing candidate alias of that same tool.
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/view-alias-candidate-collide",
                agent(
                    "collide-agent",
                    ResourceView {
                        allow: Vec::new(),
                        deny: Vec::new(),
                        aliases: BTreeMap::from([(
                            "patch".to_string(),
                            "harness:tool/apply_patch".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-view-alias-candidate-collide"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("collide-agent", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::AliasCollision { name, .. }) => {
                assert_eq!(name, "patch");
            }
            Ok(_) => panic!(
                "explicit alias occupying an existing candidate alias must typed-reject even for the same target"
            ),
            Err(other) => panic!("expected alias collision, got {other:?}"),
        }
    }

    #[test]
    fn mcp_and_skill_may_share_public_spelling_independently() {
        // Only tool versus MCP share invocation syntax. An MCP public spelling
        // and an independent skill public spelling may be identical and both
        // remain addressable.
        let local = PreparedResource {
            local_id: "pingy".to_string(),
            stable_id: "bundle:hya/mcp-skill-ok/skill/pingy".to_string(),
            source_path: "resources/skills/pingy.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("pingy", "SKILL_PINGY_BODY"),
            binary_base64: None,
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/mcp-skill-ok",
                agent(
                    "mcp-skill-ok",
                    ResourceView {
                        allow: vec![
                            "harness:mcp/mcp__fixture__ping".to_string(),
                            "bundle:hya/mcp-skill-ok/skill/pingy".to_string(),
                        ],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![RuntimeSource::new(
                    RuntimeSourceId::mcp("fixture"),
                    [5; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "ping",
                        "mcp__fixture__ping",
                        vec!["pingy".to_string()],
                        Arc::new(NoopTool::new("mcp__fixture__ping")),
                        ToolPermission::Mcp,
                    )],
                )])
            })
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-mcp-skill-ok"))
            .unwrap();
        let compiled = binding
            .compile_agent_resources(
                &binding
                    .agent_resource_policy_on_plane("mcp-skill-ok", AgentToolPlane::Full)
                    .unwrap(),
            )
            .expect("MCP and skill may share public spelling `pingy`");
        let mcp = compiled
            .resolve_tool("pingy")
            .expect("MCP public spelling `pingy` must dispatch");
        assert_eq!(mcp.tool.name(), "mcp__fixture__ping");
        assert!(
            compiled
                .skills()
                .iter()
                .any(|skill| skill.name == "pingy" && skill.content.contains("SKILL_PINGY_BODY")),
            "skill public name `pingy` must remain independently addressable"
        );
    }

    #[test]
    fn full_mcp_preserves_source_alias_under_mcp_kind() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/mcp-alias",
                agent("mcp-alias", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![RuntimeSource::new(
                    RuntimeSourceId::mcp("fixture"),
                    [3; 32],
                    Arc::new(()),
                    vec![RuntimeSourceExport::tool(
                        "ping",
                        "mcp__fixture__ping",
                        vec!["pingy".to_string()],
                        Arc::new(NoopTool::new("mcp__fixture__ping")),
                        ToolPermission::Mcp,
                    )],
                )])
            })
            .unwrap();
        let binding = registry.bind_turn(Path::new("/tmp/hya-mcp-alias")).unwrap();
        let compiled = binding
            .compile_agent_resources(
                &binding
                    .agent_resource_policy_on_plane("mcp-alias", AgentToolPlane::Full)
                    .unwrap(),
            )
            .unwrap();
        assert!(compiled.resolve_tool("mcp__fixture__ping").is_some());
        assert!(
            compiled.resolve_tool("pingy").is_some(),
            "source export alias must be a public MCP spelling"
        );
        assert!(
            compiled
                .resolve_tool("harness:mcp/mcp__fixture__ping")
                .is_some()
        );
        let schema_names = compiled
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<BTreeSet<_>>();
        assert!(schema_names.is_subset(&compiled.public_tool_names()));
        assert!(schema_names.contains("mcp__fixture__ping"));
        assert!(!schema_names.contains("pingy"));
        assert!(!schema_names.contains("harness:mcp/mcp__fixture__ping"));
    }

    #[test]
    fn prepared_skill_alias_is_public_spelling_for_skill_plane() {
        let local = PreparedResource {
            local_id: "docs".to_string(),
            stable_id: "bundle:hya/skill-alias/skill/docs".to_string(),
            source_path: "resources/skills/docs.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("docs", "DOCS_BODY"),
            binary_base64: None,
            aliases: vec!["handbook".to_string()],
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/skill-alias",
                agent(
                    "skill-alias",
                    ResourceView {
                        allow: vec!["bundle:hya/skill-alias/skill/docs".to_string()],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-skill-alias"))
            .unwrap();
        let compiled = binding
            .compile_agent_resources(&binding.agent_resource_policy("skill-alias").unwrap())
            .unwrap();
        assert!(compiled.skills().iter().any(|skill| skill.name == "docs"));
        assert!(
            compiled
                .skills()
                .iter()
                .any(|skill| skill.name == "handbook" && skill.content.contains("DOCS_BODY")),
            "PreparedResource alias must be a dispatchable skill public spelling"
        );
    }

    #[test]
    fn allow_and_deny_reject_alias_spellings() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[
                bundle_with_agent(
                    "hya/allow-alias",
                    agent(
                        "allow-alias",
                        ResourceView {
                            allow: vec!["patch".to_string()],
                            deny: Vec::new(),
                            aliases: BTreeMap::new(),
                            namespace: None,
                        },
                    ),
                    Vec::new(),
                ),
                bundle_with_agent(
                    "hya/deny-alias",
                    agent(
                        "deny-alias",
                        ResourceView {
                            allow: Vec::new(),
                            deny: vec!["patch".to_string()],
                            aliases: BTreeMap::new(),
                            namespace: None,
                        },
                    ),
                    Vec::new(),
                ),
            ])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-filter-alias"))
            .unwrap();
        for agent_id in ["allow-alias", "deny-alias"] {
            let policy = binding.agent_resource_policy(agent_id).unwrap();
            match binding.compile_agent_resources(&policy) {
                Err(BundleError::UnknownResourceReference { reference, .. }) => {
                    assert_eq!(reference, "patch");
                }
                Ok(_) => panic!("{agent_id}: alias spelling in allow/deny must typed-reject"),
                Err(other) => panic!("{agent_id}: expected unknown reference, got {other:?}"),
            }
        }
    }

    #[test]
    fn alias_cannot_impersonate_filtered_stable_name() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/filtered-stable",
                agent(
                    "filtered-stable",
                    ResourceView {
                        allow: vec!["harness:tool/read".to_string()],
                        deny: Vec::new(),
                        aliases: BTreeMap::from([(
                            "write".to_string(),
                            "harness:tool/read".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-filtered-stable"))
            .unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("filtered-stable", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::AliasCollision { name, .. }) => {
                assert_eq!(name, "write");
            }
            Ok(_) => panic!("alias must not impersonate unselected stable identity"),
            Err(other) => panic!("expected alias collision, got {other:?}"),
        }
    }

    #[test]
    fn aliased_skill_facade_drives_prompt_not_public_key_skill() {
        let local = PreparedResource {
            local_id: "bundle-skill".to_string(),
            stable_id: "bundle:hya/facade-alias/skill/bundle-skill".to_string(),
            source_path: "resources/skills/bundle-skill.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("bundle-skill", "SHOULD_NOT_INLINE"),
            binary_base64: None,
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/facade-alias",
                agent(
                    "facade-alias",
                    ResourceView {
                        allow: vec![
                            "harness:tool/skill".to_string(),
                            "bundle:hya/facade-alias/skill/bundle-skill".to_string(),
                        ],
                        deny: Vec::new(),
                        aliases: BTreeMap::from([(
                            "load_skill".to_string(),
                            "harness:tool/skill".to_string(),
                        )]),
                        namespace: None,
                    },
                ),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-facade-alias"))
            .unwrap();
        let compiled = binding
            .compile_agent_resources(
                &binding
                    .agent_resource_policy_on_plane("facade-alias", AgentToolPlane::Full)
                    .unwrap(),
            )
            .unwrap();
        assert!(compiled.resolve_tool("load_skill").is_some());
        assert!(
            compiled.resolve_tool("skill").is_none(),
            "resource-view alias replaces the ordinary short spelling of the facade"
        );
        let section = compiled
            .skills_prompt_section()
            .expect("facade selected => on-demand skill index");
        assert!(
            !section.contains("SHOULD_NOT_INLINE"),
            "aliased skill facade must not fall through to body inlining: {section}"
        );
        assert!(
            section.contains("bundle-skill"),
            "index must list the selected skill: {section}"
        );
    }

    #[test]
    fn skill_prompt_lists_all_dispatchable_spellings_with_short_preference() {
        let local = PreparedResource {
            local_id: "probe".to_string(),
            stable_id: "bundle:hya/spellings/skill/probe".to_string(),
            source_path: "resources/skills/probe.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("probe", "PROBE_BODY"),
            binary_base64: None,
            aliases: vec!["probe-alias".to_string()],
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/spellings",
                agent(
                    "spellings",
                    ResourceView {
                        allow: vec!["bundle:hya/spellings/skill/probe".to_string()],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry.bind_turn(Path::new("/tmp/hya-spellings")).unwrap();
        let compiled = binding
            .compile_agent_resources(&binding.agent_resource_policy("spellings").unwrap())
            .unwrap();
        let section = compiled
            .skills_prompt_section()
            .expect("a bundle-local-only view inlines selected local static skills");
        assert!(
            section.contains("probe"),
            "short spelling preferred in prompt: {section}"
        );
        assert!(
            section.contains("probe-alias") || section.contains("bundle:hya/spellings/skill/probe"),
            "prompt must expose additional dispatchable spellings honestly: {section}"
        );
        assert_eq!(
            section.matches("PROBE_BODY").count(),
            1,
            "content must be emitted once with a spelling list: {section}"
        );
    }

    #[test]
    fn selected_harness_skill_without_facade_fails_typed() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/no-facade",
                agent(
                    "no-facade",
                    ResourceView {
                        allow: Vec::new(),
                        deny: vec!["harness:tool/skill".to_string()],
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let workdir = tempfile_skill_workdir("workdir-only", "MUST_NOT_INLINE");
        let binding = registry.bind_turn(&workdir).unwrap();
        let policy = binding
            .agent_resource_policy_on_plane("no-facade", AgentToolPlane::Full)
            .unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::InvalidManifest { detail, .. }) => {
                assert!(
                    detail.contains("skill") || detail.contains("facade"),
                    "typed rejection must mention missing skill facade: {detail}"
                );
            }
            Err(BundleError::UnknownResourceReference { .. }) => {}
            Ok(compiled) => {
                let section = compiled.skills_prompt_section().unwrap_or_default();
                panic!(
                    "selected harness skills without facade must typed-reject, not expose content: {section}"
                );
            }
            Err(other) => panic!("expected typed facade rejection, got {other:?}"),
        }
    }

    #[test]
    fn bundle_id_with_kind_path_segments_parses_structurally() {
        let bundle_id = "hya/tool/skill/mcp-nest";
        let local = PreparedResource {
            local_id: "docs".to_string(),
            stable_id: format!("bundle:{bundle_id}/skill/docs"),
            source_path: "resources/skills/docs.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("docs", "NESTED_BUNDLE_BODY"),
            binary_base64: None,
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                bundle_id,
                agent(
                    "nested-agent",
                    ResourceView {
                        allow: vec![format!("bundle:{bundle_id}/skill/docs")],
                        deny: Vec::new(),
                        aliases: BTreeMap::new(),
                        namespace: None,
                    },
                ),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-nested-bundle-id"))
            .unwrap();
        let compiled = binding
            .compile_agent_resources(&binding.agent_resource_policy("nested-agent").unwrap())
            .expect("rightmost kind/local parse must accept nested kind segments in bundle id");
        assert!(
            compiled
                .skills()
                .iter()
                .any(|skill| skill.name == "docs" && skill.content.contains("NESTED_BUNDLE_BODY"))
        );
        assert!(
            compiled
                .skills()
                .iter()
                .any(|skill| skill.name == format!("bundle:{bundle_id}/skill/docs"))
        );
    }

    #[test]
    fn bundle_sidecar_collision_requires_provider_safe_names_for_both_targets() {
        let bundle_id = "hya/sidecar-map";
        let mut bundle = bundle_with_agent(
            bundle_id,
            agent("sidecar-agent", ResourceView::default()),
            Vec::new(),
        );
        bundle.tools.push(PreparedResource {
            local_id: "echo".to_string(),
            stable_id: format!("bundle:{bundle_id}/tool/echo"),
            source_path: "tools/echo.js".to_string(),
            digest: "test-only-digest".to_string(),
            content: "export default {}".to_string(),
            binary_base64: None,
            aliases: Vec::new(),
        });
        let catalog = Arc::new(TestCatalog::from_prepared(&[bundle]).unwrap());

        let tools = ToolRegistry::builtins();
        tools
            .register_with_permission(Arc::new(NoopTool::new("echo")), ToolPermission::Tool)
            .unwrap();
        let registry = test_runtime_registry(tools, catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-sidecar-tool-map"))
            .unwrap();
        let mut policy = binding
            .agent_resource_policy_on_plane("sidecar-agent", AgentToolPlane::Full)
            .unwrap();
        let sidecar_tool = ResolvedTool {
            tool: Arc::new(NoopTool::new(format!("bundle:{bundle_id}/tool/echo"))),
            permission: ToolPermission::Tool,
        };

        match binding.compile_agent_resources_with_sidecar_tools(
            &policy,
            std::slice::from_ref(&sidecar_tool),
        ) {
            Err(BundleError::InvalidManifest { detail, .. }) => {
                assert!(detail.contains("harness:tool/echo"));
                assert!(detail.contains("no provider-safe schema name"));
            }
            Ok(_) => panic!("selected harness Tool must not silently vanish from schemas"),
            Err(other) => panic!("expected provider-safe schema rejection, got {other:?}"),
        }

        policy
            .resource_view
            .aliases
            .insert("harness_echo".to_string(), "harness:tool/echo".to_string());
        let compiled = binding
            .compile_agent_resources_with_sidecar_tools(&policy, &[sidecar_tool])
            .expect("an explicit provider-safe alias must preserve both selected Tools");
        let schema_names = compiled
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<BTreeSet<_>>();
        assert!(schema_names.contains("echo"));
        assert!(schema_names.contains("harness_echo"));
        assert!(!schema_names.contains(&format!("bundle:{bundle_id}/tool/echo")));
        assert!(!schema_names.contains("harness:tool/echo"));

        let bundle_short = compiled.resolve_tool("echo").unwrap();
        assert_eq!(
            bundle_short.tool.name(),
            format!("bundle:{bundle_id}/tool/echo")
        );
        let bundle_qualified = compiled
            .resolve_tool(&format!("bundle:{bundle_id}/tool/echo"))
            .unwrap();
        assert_eq!(
            bundle_qualified.tool.name(),
            format!("bundle:{bundle_id}/tool/echo")
        );
        let harness_alias = compiled.resolve_tool("harness_echo").unwrap();
        assert_eq!(harness_alias.tool.name(), "echo");
        let harness_qualified = compiled.resolve_tool("harness:tool/echo").unwrap();
        assert_eq!(harness_qualified.tool.name(), "echo");
    }

    #[test]
    fn captured_agent_resource_policy_retains_disjoint_bundle_tool_and_hook_ids() {
        // One agent per bundle, so disjointness is now across two installed
        // bundles rather than across agents inside one.
        let alpha_bundle_id = "hya/disjoint-alpha";
        let beta_bundle_id = "hya/disjoint-beta";
        let alpha_tool_id = format!("bundle:{alpha_bundle_id}/tool/alpha");
        let beta_tool_id = format!("bundle:{beta_bundle_id}/tool/beta");
        let alpha_hook_id = format!("bundle:{alpha_bundle_id}/hook/event");
        let beta_hook_id = format!("bundle:{beta_bundle_id}/hook/tool.execute.before");

        let mut alpha = agent(
            "alpha-agent",
            ResourceView {
                allow: vec![alpha_tool_id.clone()],
                deny: Vec::new(),
                aliases: BTreeMap::new(),
                namespace: None,
            },
        );
        alpha.hook_refs = vec![alpha_hook_id.clone()];

        let mut beta = agent(
            "beta-agent",
            ResourceView {
                allow: vec![beta_tool_id.clone()],
                deny: Vec::new(),
                aliases: BTreeMap::new(),
                namespace: None,
            },
        );
        beta.hook_refs = vec![beta_hook_id.clone()];

        let mut alpha_bundle = bundle_with_agent(alpha_bundle_id, alpha, Vec::new());
        alpha_bundle.tools = vec![PreparedResource {
            local_id: "alpha".to_string(),
            stable_id: alpha_tool_id.clone(),
            source_path: "extensions/alpha.js".to_string(),
            digest: "alpha-tool".to_string(),
            content: "export default {}".to_string(),
            binary_base64: None,
            aliases: Vec::new(),
        }];
        alpha_bundle.hooks = vec![PreparedResource {
            local_id: "event".to_string(),
            stable_id: alpha_hook_id.clone(),
            source_path: "extensions/event.js".to_string(),
            digest: "alpha-hook".to_string(),
            content: "export default {}".to_string(),
            binary_base64: None,
            aliases: Vec::new(),
        }];

        let mut beta_bundle = bundle_with_agent(beta_bundle_id, beta, Vec::new());
        beta_bundle.tools = vec![PreparedResource {
            local_id: "beta".to_string(),
            stable_id: beta_tool_id.clone(),
            source_path: "extensions/beta.js".to_string(),
            digest: "beta-tool".to_string(),
            content: "export default {}".to_string(),
            binary_base64: None,
            aliases: Vec::new(),
        }];
        beta_bundle.hooks = vec![PreparedResource {
            local_id: "tool.execute.before".to_string(),
            stable_id: beta_hook_id.clone(),
            source_path: "extensions/before.js".to_string(),
            digest: "beta-hook".to_string(),
            content: "export default {}".to_string(),
            binary_base64: None,
            aliases: Vec::new(),
        }];

        let catalog = Arc::new(TestCatalog::from_prepared(&[alpha_bundle, beta_bundle]).unwrap());
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-disjoint-sidecars"))
            .unwrap();
        let alpha_policy = binding.agent_resource_policy("alpha-agent").unwrap();
        let beta_policy = binding.agent_resource_policy("beta-agent").unwrap();

        let alpha_selected_tools = alpha_policy
            .selected_bundle_tool_ids()
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let beta_selected_tools = beta_policy
            .selected_bundle_tool_ids()
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            alpha_selected_tools,
            BTreeSet::from([alpha_tool_id.clone()])
        );
        assert_eq!(beta_selected_tools, BTreeSet::from([beta_tool_id.clone()]));

        let alpha_hook_ids = alpha_policy
            .canonical_hook_ids()
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let beta_hook_ids = beta_policy
            .canonical_hook_ids()
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(alpha_hook_ids, BTreeSet::from([alpha_hook_id.clone()]));
        assert_eq!(beta_hook_ids, BTreeSet::from([beta_hook_id.clone()]));
        assert!(alpha_hook_ids.is_disjoint(&beta_hook_ids));

        let alpha_compiled = binding
            .compile_agent_resources_with_sidecar_tools(
                &alpha_policy,
                &[ResolvedTool {
                    tool: Arc::new(NoopTool::new(alpha_tool_id.clone())),
                    permission: ToolPermission::Tool,
                }],
            )
            .unwrap();
        let beta_compiled = binding
            .compile_agent_resources_with_sidecar_tools(
                &beta_policy,
                &[ResolvedTool {
                    tool: Arc::new(NoopTool::new(beta_tool_id.clone())),
                    permission: ToolPermission::Tool,
                }],
            )
            .unwrap();

        let alpha_compiled_hooks = alpha_compiled
            .canonical_hook_ids()
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let beta_compiled_hooks = beta_compiled
            .canonical_hook_ids()
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(alpha_compiled_hooks, alpha_hook_ids);
        assert_eq!(beta_compiled_hooks, beta_hook_ids);

        let alpha_names = domain_tool_names(&alpha_compiled);
        let beta_names = domain_tool_names(&beta_compiled);
        assert!(alpha_names.contains("alpha"));
        assert!(alpha_names.contains(alpha_tool_id.as_str()));
        assert!(!alpha_names.contains("beta"));
        assert!(!alpha_names.contains(beta_tool_id.as_str()));
        assert!(beta_names.contains("beta"));
        assert!(beta_names.contains(beta_tool_id.as_str()));
        assert!(!beta_names.contains("alpha"));
        assert!(!beta_names.contains(alpha_tool_id.as_str()));
        assert!(alpha_names.is_disjoint(&beta_names));
    }

    fn tempfile_skill_workdir(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hya-resource-view-skill-{}-{}",
            name,
            std::process::id()
        ));
        let skill_dir = path.join(".hya/skills").join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), skill_md(name, body)).unwrap();
        path
    }

    /// A tool that records every reference argument it is dispatched with, so
    /// scheme-dispatch tests can observe exactly what the owner received.
    struct ReferenceRecordingTool {
        name: String,
        seen: Mutex<Vec<String>>,
    }

    impl ReferenceRecordingTool {
        fn new(name: impl Into<String>) -> Arc<Self> {
            Arc::new(Self {
                name: name.into(),
                seen: Mutex::new(Vec::new()),
            })
        }

        fn references(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Tool for ReferenceRecordingTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new(self.name.clone()),
                description: "records dispatch references".to_string(),
                input_schema: json!({ "type": "object" }),
                output_schema: None,
            }
        }

        async fn execute(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
            if let Some(reference) = input.get("reference").and_then(Value::as_str) {
                self.seen.lock().unwrap().push(reference.to_string());
            }
            Ok(json!({ "output": "owner body" }))
        }
    }

    fn scheme_source(
        id: &str,
        exports: Vec<RuntimeSourceExport>,
        schemas: Vec<SourceSchema>,
    ) -> RuntimeSource {
        RuntimeSource::new(RuntimeSourceId::plugin(id), [0; 32], Arc::new(()), exports)
            .with_schemas(schemas)
    }

    fn dispatch_ctx(workdir: &Path) -> ToolCtx {
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
            Action::Read,
            "*",
            Mode::Allow,
        )]));
        ToolCtx {
            workflows: hya_tool::WorkflowPlane::disconnected(),
            permission,
            interaction: hya_tool::InteractionPlane::new().0,
            spawner: hya_tool::SpawnerPlane::new().0,
            operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
            mailbox: hya_tool::MailboxPlane::disconnected(),
            lifecycle: hya_tool::LifecyclePlane::disconnected(),
            session: None,
            parent_session: None,
            todo: hya_tool::TodoPlane::default(),
            skills: SkillPlane::default(),
            artifacts: hya_tool::handle::ArtifactPlane::default(),
            agents: Default::default(),
            websearch: hya_tool::WebSearchPlane::default(),
            lsp: hya_tool::LspPlane::default(),
            formatter: hya_tool::FormatterPlane::default(),
            workdir: workdir.to_path_buf(),
            roots: vec![workdir.to_path_buf()],
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }

    #[test]
    fn source_schema_claims_publish_and_dispatch_through_compiled_views() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/scheme",
                agent("scheme-agent", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let owner = ReferenceRecordingTool::new("scheme__query");
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![scheme_source(
                    "scheme-src",
                    vec![RuntimeSourceExport::tool(
                        "scheme__query",
                        "scheme__query",
                        Vec::new(),
                        owner.clone(),
                        ToolPermission::ReadOnly,
                    )],
                    vec![SourceSchema {
                        scheme: "db".to_string(),
                        canonical_tool: "scheme__query".to_string(),
                        writable: false,
                    }],
                )])
            })
            .expect("a source claiming its own export's scheme must publish");

        let workdir = PathBuf::from("/tmp/hya-scheme-dispatch");
        let binding = registry.bind_turn(&workdir).unwrap();
        let binding_binding = &binding;
        let schemes = binding_binding.schemes();
        assert_eq!(
            schemes.get("db").map(SchemeBinding::canonical_tool),
            Some("scheme__query"),
            "the winning binding must be published on the snapshot"
        );
        assert_eq!(
            binding.scheme_chain("db"),
            vec![("plugin:scheme-src".to_string(), "scheme__query".to_string())],
            "the chain lists the sole claimant"
        );
        let effective = registry.effective_schemes();
        assert_eq!(effective.schemes.len(), 1);

        let policy = binding
            .agent_resource_policy_on_plane("scheme-agent", AgentToolPlane::Full)
            .unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();

        let ctx = dispatch_ctx(&workdir);
        let read = compiled.resolve_tool("read").unwrap();
        let result = futures::executor::block_on(
            read.tool
                .execute(&ctx, json!({ "path": "db://x/y?head=2" })),
        )
        .unwrap();
        assert_eq!(
            result.get("output").and_then(Value::as_str),
            Some("owner body"),
            "read over a registered scheme dispatches the owner tool"
        );
        assert_eq!(
            owner.references(),
            vec!["db://x/y?head=2".to_string()],
            "the owner receives the full handle text as its reference"
        );

        let write = compiled.resolve_tool("write").unwrap();
        let error = futures::executor::block_on(
            write
                .tool
                .execute(&ctx, json!({ "path": "db://x/y", "content": "row" })),
        )
        .expect_err("a read-only scheme must reject write dispatch");
        assert!(
            error.to_string().contains("db:// is read-only"),
            "write dispatch must fail with the NotWritable-style error: {error}"
        );
        assert!(
            owner.references().len() == 1,
            "the rejected write must not reach the owner"
        );
    }

    #[test]
    fn two_sources_claiming_one_scheme_mask_by_greater_source_id() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/scheme-mask",
                agent("scheme-mask-agent", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let exports = |name: &str| {
            vec![RuntimeSourceExport::tool(
                name,
                name,
                Vec::new(),
                Arc::new(NoopTool::new(name)),
                ToolPermission::ReadOnly,
            )]
        };
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![
                    scheme_source(
                        "aaa-src",
                        exports("aaa__query"),
                        vec![SourceSchema {
                            scheme: "db".to_string(),
                            canonical_tool: "aaa__query".to_string(),
                            writable: false,
                        }],
                    ),
                    scheme_source(
                        "zzz-src",
                        exports("zzz__query"),
                        vec![SourceSchema {
                            scheme: "db".to_string(),
                            canonical_tool: "zzz__query".to_string(),
                            writable: true,
                        }],
                    ),
                ])
            })
            .expect("two sources may contest one scheme");

        let binding = registry
            .bind_turn(&PathBuf::from("/tmp/hya-scheme-mask"))
            .unwrap();
        let winner = binding.schemes().get("db").expect("scheme must resolve");
        assert_eq!(winner.owner(), "plugin:zzz-src");
        assert_eq!(winner.canonical_tool(), "zzz__query");
        assert!(winner.writable());
        assert_eq!(
            binding.scheme_chain("db"),
            vec![
                ("plugin:aaa-src".to_string(), "aaa__query".to_string()),
                ("plugin:zzz-src".to_string(), "zzz__query".to_string()),
            ],
            "the chain lists every claimant in ascending source id order"
        );
    }

    #[test]
    fn foreign_tool_and_protected_scheme_claims_are_one_grouped_error() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/scheme-bad",
                agent("scheme-bad-agent", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let error = registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![
                    scheme_source(
                        "bad-a",
                        vec![RuntimeSourceExport::tool(
                            "bad-a__query",
                            "bad-a__query",
                            Vec::new(),
                            Arc::new(NoopTool::new("bad-a__query")),
                            ToolPermission::ReadOnly,
                        )],
                        vec![SourceSchema {
                            scheme: "db".to_string(),
                            canonical_tool: "not-exported__tool".to_string(),
                            writable: false,
                        }],
                    ),
                    scheme_source(
                        "bad-b",
                        Vec::new(),
                        vec![
                            SourceSchema {
                                scheme: "local".to_string(),
                                canonical_tool: "bad-b__put".to_string(),
                                writable: true,
                            },
                            SourceSchema {
                                scheme: "d".to_string(),
                                canonical_tool: "bad-b__put".to_string(),
                                writable: true,
                            },
                        ],
                    ),
                ])
            })
            .expect_err("invalid scheme claims must be rejected");
        let RuntimeRefreshError::SchemaConflicts(report) = error else {
            panic!("expected a grouped schema conflict report");
        };
        for fragment in [
            "plugin:bad-a",
            "not-exported__tool",
            "plugin:bad-b",
            "internal scheme `local`",
            "scheme `d`",
        ] {
            assert!(
                report.contains(fragment),
                "grouped report must contain {fragment:?}:\n{report}"
            );
        }
    }

    #[test]
    fn scheme_dispatch_stays_absent_when_the_view_lacks_the_owner() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/scheme-scope",
                agent("scheme-scope-agent", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let owner = ReferenceRecordingTool::new("scheme__query");
        registry
            .refresh(|candidate| {
                candidate.upsert_sources(vec![scheme_source(
                    "scheme-src",
                    vec![RuntimeSourceExport::tool(
                        "scheme__query",
                        "scheme__query",
                        Vec::new(),
                        owner.clone(),
                        ToolPermission::ReadOnly,
                    )],
                    vec![SourceSchema {
                        scheme: "db".to_string(),
                        canonical_tool: "scheme__query".to_string(),
                        writable: false,
                    }],
                )])
            })
            .expect("a valid scheme claim must publish");

        let workdir = PathBuf::from("/tmp/hya-scheme-scope");
        let binding = registry.bind_turn(&workdir).unwrap();
        // The bundle agent's InternalPublic plane never contains plugin tools,
        // so the owning tool is absent from this view.
        let policy = binding
            .agent_resource_policy_on_plane("scheme-scope-agent", AgentToolPlane::InternalPublic)
            .unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        assert!(
            compiled.resolve_tool("scheme__query").is_none(),
            "the owner tool must be absent from the restricted view"
        );

        let ctx = dispatch_ctx(&workdir);
        let read = compiled.resolve_tool("read").unwrap();
        let error =
            futures::executor::block_on(read.tool.execute(&ctx, json!({ "path": "db://x/y" })))
                .expect_err("a view without the owner keeps the historical unknown-scheme error");
        assert!(
            error.to_string().contains("unknown handle scheme"),
            "no global fallback may dispatch outside the view: {error}"
        );
        assert!(
            owner.references().is_empty(),
            "the owner tool must never be invoked from a view it is not in"
        );
    }

    fn bundle_scheme_source(bundle_id: &str, scheme: &str, canonical_tool: &str) -> RuntimeSource {
        RuntimeSource::new(
            RuntimeSourceId::bundle(bundle_id),
            [7; 32],
            Arc::new(()),
            Vec::new(),
        )
        .with_schemas(vec![SourceSchema {
            scheme: scheme.to_string(),
            canonical_tool: canonical_tool.to_string(),
            writable: false,
        }])
    }

    /// A Bundle source exports no registry tools, so its schema claim names the
    /// owning tool by its view-scoped stable id; publication must still admit it.
    #[test]
    fn bundle_source_schema_claim_publishes_without_registry_exports() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/vecdb",
                agent("vecdb-lead", ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| {
                candidate.replace_sources_of_kind(
                    RuntimeSourceKind::Bundle,
                    vec![bundle_scheme_source(
                        "hya/vecdb",
                        "db",
                        "bundle:hya/vecdb/tool/query",
                    )],
                )
            })
            .expect("a bundle schema claim on its own stable-id tool must publish");

        let binding = registry
            .bind_turn(&PathBuf::from("/tmp/hya-bundle-scheme"))
            .unwrap();
        let winner = binding.schemes().get("db").expect("scheme must resolve");
        assert_eq!(winner.owner(), "bundle:hya/vecdb");
        assert_eq!(winner.canonical_tool(), "bundle:hya/vecdb/tool/query");
        assert_eq!(
            binding.scheme_chain("db"),
            vec![(
                "bundle:hya/vecdb".to_string(),
                "bundle:hya/vecdb/tool/query".to_string(),
            )],
            "the chain lists the bundle claimant with its stable-id tool"
        );
    }

    /// A bundle agent whose view selects both the owner tool and the scheme
    /// reads `scheme://` handles through the view's own sidecar tool.
    #[test]
    fn bundle_agent_view_with_sidecar_owner_reads_registered_scheme() {
        let view = ResourceView {
            allow: vec!["query".to_string(), "harness:tool/read".to_string()],
            deny: Vec::new(),
            aliases: BTreeMap::new(),
            namespace: None,
        };
        let mut bundle = bundle_with_agent("hya/vecdb", agent("vecdb-lead", view), Vec::new());
        bundle.tools.push(PreparedResource {
            local_id: "query".to_string(),
            stable_id: "bundle:hya/vecdb/tool/query".to_string(),
            source_path: "extensions/runtime.js".to_string(),
            digest: "test-only".to_string(),
            content: "export default {}".to_string(),
            binary_base64: None,
            aliases: Vec::new(),
        });
        let catalog = Arc::new(TestCatalog::from_prepared(&[bundle]).unwrap());
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| {
                candidate.replace_sources_of_kind(
                    RuntimeSourceKind::Bundle,
                    vec![bundle_scheme_source(
                        "hya/vecdb",
                        "db",
                        "bundle:hya/vecdb/tool/query",
                    )],
                )
            })
            .expect("the bundle claim must publish");

        let workdir = PathBuf::from("/tmp/hya-bundle-scheme-dispatch");
        let binding = registry.bind_turn(&workdir).unwrap();
        let policy = binding.agent_resource_policy("vecdb-lead").unwrap();
        let owner = ReferenceRecordingTool::new("bundle:hya/vecdb/tool/query");
        let sidecar = ResolvedTool {
            tool: owner.clone(),
            permission: ToolPermission::Tool,
        };
        let compiled = binding
            .compile_agent_resources_with_sidecar_tools(&policy, &[sidecar])
            .unwrap();

        let ctx = dispatch_ctx(&workdir);
        let read = compiled.resolve_tool("read").unwrap();
        let result =
            futures::executor::block_on(read.tool.execute(&ctx, json!({ "path": "db://rows/42" })))
                .unwrap();
        assert_eq!(
            result.get("output").and_then(Value::as_str),
            Some("owner body"),
            "read over the registered scheme dispatches the view's sidecar tool"
        );
        assert_eq!(
            owner.references(),
            vec!["db://rows/42".to_string()],
            "the owner receives the full handle text as its reference"
        );
    }

    /// Whether `name` spells a harness coordination tool (short or qualified).
    fn is_coordination_spelling(name: &str) -> bool {
        let short = name.strip_prefix("harness:tool/").unwrap_or(name);
        crate::coordination::COORDINATION_TOOLS.contains(&short)
    }

    /// Whether the view's `read` is the injected mail-only channel reader.
    fn has_mail_only_read(compiled: &CompiledResourceView) -> bool {
        compiled
            .resolve_tool("read")
            .is_some_and(|read| read.tool.schema().description.starts_with("Read team mail"))
    }

    /// Public tool names minus the harness-injected coordination set: the
    /// domain tools a `resource_view` actually narrows.
    fn domain_tool_names(compiled: &CompiledResourceView) -> BTreeSet<String> {
        let mail_only_read = has_mail_only_read(compiled);
        compiled
            .public_tool_names()
            .into_iter()
            .filter(|name| !is_coordination_spelling(name) && !(mail_only_read && name == "read"))
            .collect()
    }

    /// Model-facing schema names minus the harness-injected coordination set.
    fn domain_schema_names(compiled: &CompiledResourceView) -> BTreeSet<String> {
        let mail_only_read = has_mail_only_read(compiled);
        compiled
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .filter(|name| !is_coordination_spelling(name) && !(mail_only_read && name == "read"))
            .collect()
    }

    // ---- Coordination tools (0.41.0): harness-owned, injected at startup ----

    const WITHOUT_CHANNEL_TOOLS: [&str; 4] = [
        "hya/base-tools",
        "hya/extended-tools",
        "hya/network-tools",
        "hya/todo-tools",
    ];

    fn view(allow: &[&str], deny: &[&str]) -> ResourceView {
        ResourceView {
            allow: allow.iter().map(|entry| (*entry).to_string()).collect(),
            deny: deny.iter().map(|entry| (*entry).to_string()).collect(),
            aliases: BTreeMap::new(),
            namespace: None,
        }
    }

    /// Compile `agent` (installed as its own bundle) and return the view.
    fn compile_bundle_agent(
        tools: ToolRegistry,
        agent: PreparedAgent,
    ) -> Result<Arc<CompiledResourceView>, BundleError> {
        let stable_id = agent.id.as_str().to_string();
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                &format!("hya/coord-{stable_id}"),
                agent,
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = test_runtime_registry(tools, catalog);
        let binding = registry
            .bind_turn(&PathBuf::from("/tmp/hya-coordination-tools"))
            .unwrap();
        let policy = binding.agent_resource_policy(&stable_id)?;
        binding.compile_agent_resources(&policy)
    }

    fn schema_names(compiled: &CompiledResourceView) -> BTreeSet<String> {
        compiled
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect()
    }

    #[test]
    fn coordination_tools_join_a_narrow_bundle_view() {
        let compiled = compile_bundle_agent(
            ToolRegistry::builtins(),
            agent("narrow-scout", view(&["harness:tool/grep"], &[])),
        )
        .unwrap();
        let names = schema_names(&compiled);
        for expected in ["grep", "report", "wait", "send", "list_channel", "read"] {
            assert!(names.contains(expected), "missing `{expected}`: {names:?}");
        }
        for absent in ["task", "archive", "bash", "glob", "edit", "write"] {
            assert!(!names.contains(absent), "`{absent}` leaked: {names:?}");
        }
    }

    #[test]
    fn injected_read_serves_channel_mail_but_never_files() {
        let compiled = compile_bundle_agent(
            ToolRegistry::builtins(),
            agent("mail-reader", view(&["harness:tool/grep"], &[])),
        )
        .unwrap();
        let schema = compiled
            .tool_schemas()
            .into_iter()
            .find(|schema| schema.name.as_str() == "read")
            .expect("channel read is advertised");
        assert!(
            schema.description.contains("channel://"),
            "{}",
            schema.description
        );
        let read = compiled.resolve_tool("read").unwrap();
        let ctx = dispatch_ctx(Path::new("/tmp/hya-coordination-tools"));
        let error =
            futures::executor::block_on(read.tool.execute(&ctx, json!({ "path": "Cargo.toml" })))
                .unwrap_err();
        assert!(
            matches!(&error, ToolError::Input(message) if message.contains("channel://")),
            "a file path must be refused with the channel spelling: {error:?}"
        );
    }

    #[test]
    fn a_view_that_selects_read_keeps_the_full_read_tool() {
        let compiled = compile_bundle_agent(
            ToolRegistry::builtins(),
            agent("file-reader", view(&["harness:tool/read"], &[])),
        )
        .unwrap();
        let read = compiled
            .tool_schemas()
            .into_iter()
            .find(|schema| schema.name.as_str() == "read")
            .unwrap();
        assert!(
            read.description.starts_with("Read a file"),
            "{}",
            read.description
        );
    }

    #[test]
    fn spawn_rights_add_task_and_archive() {
        let compiled = compile_bundle_agent(
            ToolRegistry::builtins(),
            PreparedAgent {
                can_spawn: vec![AgentName::new("explore")],
                ..agent("narrow-lead", view(&["harness:tool/grep"], &[]))
            },
        )
        .unwrap();
        let names = schema_names(&compiled);
        assert!(
            names.contains("task") && names.contains("archive"),
            "{names:?}"
        );
    }

    #[test]
    fn a_default_view_without_spawn_rights_has_no_task_or_archive() {
        let compiled = compile_bundle_agent(
            ToolRegistry::builtins(),
            agent("leaf-worker", ResourceView::default()),
        )
        .unwrap();
        let names = schema_names(&compiled);
        assert!(
            names.contains("bash") && names.contains("wait"),
            "{names:?}"
        );
        assert!(
            !names.contains("task") && !names.contains("archive"),
            "an agent that can spawn nobody must not see task/archive: {names:?}"
        );
    }

    #[test]
    fn without_channel_tools_no_channel_tools_are_injected() {
        let compiled = compile_bundle_agent(
            ToolRegistry::from_tool_families(&WITHOUT_CHANNEL_TOOLS),
            agent("quiet-scout", view(&["harness:tool/grep"], &[])),
        )
        .unwrap();
        let names = schema_names(&compiled);
        assert_eq!(
            names,
            ["grep", "wait"].into_iter().map(str::to_string).collect(),
            "only the extended-tools wait joins when the channel family is absent"
        );
    }

    #[test]
    fn explicit_coordination_entries_dedupe_with_the_injected_set() {
        let compiled = compile_bundle_agent(
            ToolRegistry::builtins(),
            agent(
                "explicit-scout",
                view(
                    &[
                        "harness:tool/grep",
                        "harness:tool/report",
                        "harness:tool/wait",
                        "list_channel",
                    ],
                    &[],
                ),
            ),
        )
        .unwrap();
        let schemas = compiled.tool_schemas();
        for name in ["report", "wait", "list_channel"] {
            assert_eq!(
                schemas
                    .iter()
                    .filter(|schema| schema.name.as_str() == name)
                    .count(),
                1,
                "`{name}` must be advertised exactly once"
            );
        }
    }

    #[test]
    fn deny_removes_a_safe_coordination_tool() {
        let compiled = compile_bundle_agent(
            ToolRegistry::builtins(),
            PreparedAgent {
                can_spawn: vec![AgentName::new("explore")],
                ..agent(
                    "no-task-lead",
                    view(&["harness:tool/grep"], &["harness:tool/task", "send"]),
                )
            },
        )
        .unwrap();
        let names = schema_names(&compiled);
        assert!(
            !names.contains("task") && !names.contains("send"),
            "{names:?}"
        );
        assert!(
            names.contains("archive") && names.contains("report"),
            "{names:?}"
        );
    }

    #[test]
    fn deny_of_report_is_rejected() {
        let error = compile_bundle_agent(
            ToolRegistry::builtins(),
            agent("stuck-scout", view(&[], &["harness:tool/report"])),
        )
        .err()
        .expect("denying report must be rejected");
        assert!(
            matches!(&error, BundleError::InvalidManifest { detail, .. } if detail.contains("report")),
            "{error:?}"
        );
    }

    #[test]
    fn deny_of_read_keeps_the_channel_mail_path() {
        let compiled = compile_bundle_agent(
            ToolRegistry::builtins(),
            agent("no-files", view(&[], &["harness:tool/read"])),
        )
        .unwrap();
        let read = compiled
            .tool_schemas()
            .into_iter()
            .find(|schema| schema.name.as_str() == "read")
            .expect("channel read survives a read deny");
        assert!(
            read.description.contains("channel://"),
            "{}",
            read.description
        );
    }

    #[test]
    fn builtin_agents_keep_every_coordination_tool() {
        let catalog = Arc::new(TestCatalog::from_prepared(&[]).unwrap());
        let registry = test_runtime_registry(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(&PathBuf::from("/tmp/hya-coordination-tools"))
            .unwrap();
        let policy = binding.agent_resource_policy("build").unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        let names = schema_names(&compiled);
        for expected in [
            "task",
            "archive",
            "wait",
            "report",
            "send",
            "list_channel",
            "read",
        ] {
            assert!(names.contains(expected), "missing `{expected}`: {names:?}");
        }
    }
}
