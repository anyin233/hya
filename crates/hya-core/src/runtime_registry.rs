use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use hya_bundle::{BundleCatalog, BundleError, ExportKind, ResourceView};

use crate::agent_catalog::{AgentCatalog, AgentDefinition, AgentOrigin};
use hya_proto::{ConfigGeneration, ToolName, ToolSchema};
use hya_tool::{
    DuplicateName, PermissionPlane, ResolvedTool, SkillCatalogEntry, SkillPlane, Tool,
    ToolPermission, ToolRegistry, ToolRegistrySnapshot, discover_skills, parse_skill,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

const RUNTIME_SOURCE_DISPATCH_IDENTITY_DOMAIN_V1: &[u8] = b"hya.core.runtime-source-dispatch/v1";
const RUNTIME_SEMANTIC_FINGERPRINT_DOMAIN_V2: &[u8] = b"hya.core.runtime-semantic-fingerprint/v2";

/// A complete immutable configuration view. Turns retain its `Arc` for their
/// whole lifetime, so publication cannot alter an in-flight lookup.
struct RuntimeSnapshot {
    generation: ConfigGeneration,
    catalog: Arc<AgentCatalog>,
    basic_tools: ToolRegistrySnapshot,
    tools: ToolRegistrySnapshot,
    skills: BTreeMap<PathBuf, Arc<Vec<SkillCatalogEntry>>>,
    sources: BTreeMap<RuntimeSourceId, RuntimeSource>,
}

/// The sole owner and publisher of the effective tool/skill/MCP runtime view.
///
/// Candidate construction is serialized but never holds the active pointer
/// lock. Publication is one `Arc` replacement; bound-turn dispatch reads no
/// registry lock.
pub struct RuntimeRegistry {
    publication: Mutex<()>,
    active: RwLock<Arc<RuntimeSnapshot>>,
}

/// Offline mutable candidate. Its contents cannot become effective except
/// through [`RuntimeRegistry::refresh`].
pub struct RuntimeCandidate {
    tools: ToolRegistry,
    skills: BTreeMap<PathBuf, Arc<Vec<SkillCatalogEntry>>>,
    sources: BTreeMap<RuntimeSourceId, RuntimeSource>,
}

/// Kind of external runtime contribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeSourceKind {
    /// MCP server tools.
    Mcp,
    /// Plugin-declared tools.
    Plugin,
}

/// Stable identity of a configured MCP/plugin source.
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

/// Published MCP/plugin source with tools and opaque resources.
#[derive(Clone)]
pub struct RuntimeSource {
    id: RuntimeSourceId,
    declaration_digest: [u8; 32],
    owner: Arc<dyn RuntimeSourceOwner>,
    exports: Vec<RuntimeSourceExport>,
    resources: Arc<BTreeMap<String, Value>>,
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

/// One admitted turn's immutable runtime binding.
#[derive(Clone)]
pub struct TurnBinding {
    snapshot: Arc<RuntimeSnapshot>,
    workdir: PathBuf,
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
    selected_bundle_tool_ids: Arc<[String]>,
    canonical_hook_ids: Arc<[String]>,
}

impl AgentResourcePolicy {
    /// Bundle-local tool ids selected for this agent view.
    #[must_use]
    pub fn selected_bundle_tool_ids(&self) -> &[String] {
        self.selected_bundle_tool_ids.as_ref()
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
    /// Config generation counter overflowed.
    #[error("configuration generation exhausted")]
    GenerationExhausted,
    /// Candidate failed structural validation.
    #[error("invalid runtime candidate: {0}")]
    InvalidCandidate(String),
}

impl RuntimeRegistry {
    /// Start a registry from a builder tools map and bundle catalog.
    #[must_use]
    pub fn new(tools: ToolRegistry, catalog: Arc<AgentCatalog>) -> Self {
        Self::from_snapshot(tools.snapshot(), catalog)
    }

    /// Start a registry from a frozen tool snapshot.
    #[must_use]
    pub fn from_snapshot(tools: ToolRegistrySnapshot, catalog: Arc<AgentCatalog>) -> Self {
        Self {
            publication: Mutex::new(()),
            active: RwLock::new(Arc::new(RuntimeSnapshot {
                generation: ConfigGeneration::INITIAL,
                catalog,
                basic_tools: tools.clone(),
                tools,
                skills: BTreeMap::new(),
                sources: BTreeMap::new(),
            })),
        }
    }

    /// Capture the complete view for one admitted turn. Skill discovery is
    /// performed once before capture; a logically unchanged result is a no-op.
    pub fn bind_turn(&self, workdir: &Path) -> Result<TurnBinding, RuntimeRefreshError> {
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = self.active();
        let discovered = discover_skills(workdir);
        let existing = current
            .skills
            .get(workdir)
            .map_or(&[][..], |skills| skills.as_slice());
        if existing == discovered {
            return Ok(TurnBinding {
                snapshot: current,
                workdir: workdir.to_path_buf(),
            });
        }

        let mut candidate = RuntimeCandidate::from_snapshot(&current);
        candidate.replace_skills(workdir, discovered);
        let published = self.publish_candidate(current, candidate)?;
        Ok(TurnBinding {
            snapshot: published,
            workdir: workdir.to_path_buf(),
        })
    }

    /// Build and validate a complete candidate, then publish it with one pointer
    /// replacement. Failed candidates do not allocate a generation.
    pub fn refresh(
        &self,
        build: impl FnOnce(&mut RuntimeCandidate) -> Result<(), RuntimeRefreshError>,
    ) -> Result<ConfigGeneration, RuntimeRefreshError> {
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = self.active();
        let mut candidate = RuntimeCandidate::from_snapshot(&current);
        build(&mut candidate)?;
        if candidate.logically_matches(&current) {
            return Ok(current.generation);
        }
        Ok(self.publish_candidate(current, candidate)?.generation)
    }

    /// Atomically publish a complete agent catalog while preserving the
    /// current tool, skill, and source view.
    pub fn publish_catalog(
        &self,
        catalog: Arc<AgentCatalog>,
    ) -> Result<ConfigGeneration, RuntimeRefreshError> {
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = self.active();
        if current.catalog.bundles().bundles() == catalog.bundles().bundles() {
            return Ok(current.generation);
        }
        let generation = current
            .generation
            .checked_next()
            .ok_or(RuntimeRefreshError::GenerationExhausted)?;
        let published = Arc::new(RuntimeSnapshot {
            generation,
            catalog,
            basic_tools: current.basic_tools.clone(),
            tools: current.tools.clone(),
            skills: current.skills.clone(),
            sources: current.sources.clone(),
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

    fn publish_candidate(
        &self,
        current: Arc<RuntimeSnapshot>,
        candidate: RuntimeCandidate,
    ) -> Result<Arc<RuntimeSnapshot>, RuntimeRefreshError> {
        let tools = candidate.tools.snapshot();
        let skills = candidate.skills;
        let generation = current
            .generation
            .checked_next()
            .ok_or(RuntimeRefreshError::GenerationExhausted)?;
        let published = Arc::new(RuntimeSnapshot {
            generation,
            catalog: Arc::clone(&current.catalog),
            basic_tools: current.basic_tools.clone(),
            tools,
            skills,
            sources: candidate.sources,
        });
        *self
            .active
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = published.clone();
        Ok(published)
    }
}

impl RuntimeCandidate {
    fn from_snapshot(snapshot: &RuntimeSnapshot) -> Self {
        Self {
            tools: ToolRegistry::from_snapshot(&snapshot.tools),
            skills: snapshot.skills.clone(),
            sources: snapshot.sources.clone(),
        }
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
        self.replace_skills(workdir, discover_skills(workdir));
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
                let identity = runtime_source_dispatch_identity(&source, export)?;
                self.tools
                    .register_with_permission_and_aliases_and_dispatch_identity(
                        export.tool.clone(),
                        export.permission,
                        &export.aliases,
                        identity,
                    )?;
            }
            self.sources.insert(source.id.clone(), source);
        }
        Ok(())
    }

    /// Remove sources by id from this candidate.
    pub fn remove_sources(&mut self, removed: &BTreeSet<RuntimeSourceId>) {
        for id in removed {
            if let Some(source) = self.sources.remove(id) {
                for export in source.exports {
                    self.tools.remove(&export.canonical_name);
                }
            }
        }
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
        self.tools.logically_matches(&snapshot.tools)
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

impl RuntimeSource {
    /// Build a published source with exports and an empty resource map.
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
            resources: Arc::new(BTreeMap::new()),
        }
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
                    && left.resources == right.resources
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

impl TurnBinding {
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
            .get(&self.workdir)
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

        Some(Sha256::digest(bytes).into())
    }

    #[must_use]
    /// Config generation of the retained snapshot.
    pub fn generation(&self) -> ConfigGeneration {
        self.snapshot.generation
    }

    #[must_use]
    /// Working directory this turn was bound to.
    pub fn workdir(&self) -> &Path {
        &self.workdir
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
    /// Look up an agent by stable id, whatever its origin.
    pub fn resolve_agent(&self, stable_id: &str) -> Option<AgentDefinition<'_>> {
        self.snapshot.catalog.resolve(stable_id)
    }

    /// Resolve a user/model agent request against the catalog.
    pub fn resolve_requested_agent(
        &self,
        requested: Option<&str>,
    ) -> Result<AgentDefinition<'_>, BundleError> {
        self.snapshot.catalog.require(requested.unwrap_or("general"))
    }

    /// Resolve whether `caller` may spawn `target`.
    pub fn resolve_spawn(
        &self,
        caller: &str,
        requested: &str,
    ) -> Result<AgentDefinition<'_>, BundleError> {
        self.snapshot.catalog.resolve_spawn(caller, requested)
    }

    /// Agents the caller may spawn per can_spawn rules.
    pub fn spawnable_agents(&self, caller: &str) -> Result<Vec<AgentDefinition<'_>>, BundleError> {
        self.snapshot.catalog.spawnable(caller)
    }

    /// Compile the agent resource/tool policy for `stable_id`.
    ///
    /// The Harness plane is derived from the agent's origin. A built-in gets
    /// [`AgentToolPlane::Full`]; an installed bundle agent gets
    /// [`AgentToolPlane::InternalPublic`] and its own bundle resources. There is
    /// no manifest field that can change this.
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
        let mut policy = AgentResourcePolicy {
            bundle_id,
            plane,
            resource_view,
            selected_bundle_tool_ids: Arc::from(Vec::<String>::new()),
            canonical_hook_ids: Arc::from(hook_refs),
        };
        if let Ok(partitions) = self.collect_resource_candidates(&policy)
            && let Ok(selected) = select_candidates_globally(&policy, &partitions)
        {
            policy.selected_bundle_tool_ids = Arc::from(
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
                        local_id: resource.local_id.clone(),
                        source_path: resource.source_path.clone(),
                        content: resource.content.clone(),
                        short_name: resource.local_id.clone(),
                        qualified_name: resource.stable_id.clone(),
                        aliases: resource.aliases.clone(),
                    },
                );
            }
            collect_bundle_skill_candidates(bundles, bundle_id, &mut skill_candidates)?;
            collect_bundle_mcp_candidates(bundles, bundle_id, &mut mcp_candidates)?;
        }

        collect_harness_tool_candidates(
            policy.plane,
            &self.snapshot.basic_tools,
            &self.snapshot.tools,
            &self.snapshot.sources,
            &mut tool_candidates,
        );
        collect_harness_skill_candidates(policy.plane, self.skills(), &mut skill_candidates);
        collect_harness_mcp_candidates(policy.plane, &self.snapshot.sources, &mut mcp_candidates);

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

        if selected.mcp.iter().any(|id| {
            partitions
                .mcp
                .get(id)
                .is_some_and(ResourceCandidate::is_bundle_local)
        }) {
            return Err(BundleError::UnsupportedBundleFeature {
                bundle_id: bundle_id.to_string(),
                feature: "resources.mcp".to_string(),
            });
        }

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
                    if let Some(resolved) = sidecar_tools_by_name.get(canonical_id) {
                        tools.insert(public_name.clone(), resolved.clone());
                    } else {
                        return Err(BundleError::UnsupportedBundleFeature {
                            bundle_id: bundle_id.to_string(),
                            feature: "resources.tools".to_string(),
                        });
                    }
                }
                ResourceCandidate::BundleLocal { .. } => {
                    return Err(BundleError::UnsupportedBundleFeature {
                        bundle_id: bundle_id.to_string(),
                        feature: "resources.mcp".to_string(),
                    });
                }
                ResourceCandidate::HarnessTool { resolved, .. }
                | ResourceCandidate::HarnessMcp { resolved, .. } => {
                    tools.insert(public_name.clone(), resolved.clone());
                }
                ResourceCandidate::HarnessSkill { .. } => {
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
                ResourceCandidate::BundleLocal {
                    local_id,
                    source_path,
                    content,
                    ..
                } => parse_bundle_skill(bundle_id, local_id, source_path, content)?,
                ResourceCandidate::HarnessSkill { entry, .. } => entry.clone(),
                ResourceCandidate::HarnessTool { .. } | ResourceCandidate::HarnessMcp { .. } => {
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

        let schemas = tools
            .iter()
            .map(|(public_name, resolved)| {
                let mut schema = resolved.tool.schema();
                schema.name = ToolName::new(public_name.clone());
                schema
            })
            .collect();

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
            .get(&self.workdir)
            .map_or(&[], |skills| skills.as_slice())
    }

    #[must_use]
    /// Build a skill plane over this view's skill snapshot.
    pub fn skill_plane(&self) -> SkillPlane {
        let skills = self
            .snapshot
            .skills
            .get(&self.workdir)
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
        local_id: String,
        source_path: String,
        content: String,
        short_name: String,
        qualified_name: String,
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
            | Self::HarnessTool { short_name, .. }
            | Self::HarnessSkill { short_name, .. }
            | Self::HarnessMcp { short_name, .. } => short_name,
        }
    }

    fn qualified_name(&self) -> &str {
        match self {
            Self::BundleLocal { qualified_name, .. }
            | Self::HarnessTool { qualified_name, .. }
            | Self::HarnessSkill { qualified_name, .. }
            | Self::HarnessMcp { qualified_name, .. } => qualified_name,
        }
    }

    fn is_bundle_local(&self) -> bool {
        matches!(self, Self::BundleLocal { .. })
    }

    fn aliases(&self) -> &[String] {
        match self {
            Self::BundleLocal { aliases, .. }
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
                local_id: resource.local_id.clone(),
                source_path: resource.source_path.clone(),
                content: resource.content.clone(),
                short_name: resource.local_id.clone(),
                qualified_name: resource.stable_id.clone(),
                aliases: resource.aliases.clone(),
            },
        );
    }
    Ok(())
}

fn collect_harness_tool_candidates(
    plane: AgentToolPlane,
    basic_tools: &ToolRegistrySnapshot,
    full_tools: &ToolRegistrySnapshot,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    // `basic_tools` is the snapshot captured when the registry was built, before
    // any MCP or plugin publication. That is what makes it the internal public
    // plane: later contributions cannot reach it.
    let selected = match plane {
        AgentToolPlane::InternalPublic => basic_tools,
        AgentToolPlane::Full => full_tools,
    };
    let mcp_canonical_names = sources
        .values()
        .filter(|source| source.id.kind() == RuntimeSourceKind::Mcp)
        .flat_map(|source| {
            source
                .exports
                .iter()
                .map(|export| export.canonical_name.as_str())
        })
        .collect::<BTreeSet<_>>();
    let mut entries = selected.canonical_tools();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, resolved) in entries {
        // MCP is an independent resource kind; do not re-home it under tool.
        if mcp_canonical_names.contains(name.as_str()) {
            continue;
        }
        let aliases = selected.aliases_for_canonical(&name);
        let qualified = harness_id("tool", &name);
        out.insert(
            qualified.clone(),
            ResourceCandidate::HarnessTool {
                short_name: name,
                qualified_name: qualified,
                resolved,
                aliases,
            },
        );
    }
}

fn collect_bundle_skill_candidates(
    catalog: &BundleCatalog,
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
    for resource in resources {
        out.insert(
            resource.stable_id.clone(),
            ResourceCandidate::BundleLocal {
                local_id: resource.local_id.clone(),
                source_path: resource.source_path.clone(),
                content: resource.content.clone(),
                short_name: resource.local_id.clone(),
                qualified_name: resource.stable_id.clone(),
                aliases: resource.aliases.clone(),
            },
        );
    }
    Ok(())
}

fn collect_harness_skill_candidates(
    plane: AgentToolPlane,
    harness_skills: &[SkillCatalogEntry],
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    // Project and user skills are discovered from the working directory. A
    // bundle agent must not see them.
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
                local_id: resource.local_id.clone(),
                source_path: resource.source_path.clone(),
                content: resource.content.clone(),
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
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    // MCP servers are configured at the Harness level. A bundle agent gets only
    // the MCP declarations its own bundle ships.
    if plane != AgentToolPlane::Full {
        return;
    }
    let mut exports = sources
        .values()
        .filter(|source| source.id.kind() == RuntimeSourceKind::Mcp)
        .flat_map(|source| source.exports.iter())
        .collect::<Vec<_>>();
    exports.sort_by(|left, right| left.canonical_name.cmp(&right.canonical_name));
    for export in exports {
        let qualified = harness_id("mcp", &export.canonical_name);
        out.insert(
            qualified.clone(),
            ResourceCandidate::HarnessMcp {
                short_name: export.canonical_name.clone(),
                qualified_name: qualified,
                resolved: ResolvedTool {
                    tool: export.tool.clone(),
                    permission: export.permission,
                },
                aliases: export.aliases.clone(),
            },
        );
    }
}

fn parse_bundle_skill(
    bundle_id: &str,
    local_id: &str,
    source_path: &str,
    content: &str,
) -> Result<SkillCatalogEntry, BundleError> {
    let parsed = parse_skill(content).ok_or_else(|| BundleError::InvalidManifest {
        source_name: source_path.to_string(),
        detail: "bundle-local skill must contain valid SKILL.md frontmatter".to_string(),
    })?;
    if parsed.name != local_id {
        return Err(BundleError::InvalidManifest {
            source_name: source_path.to_string(),
            detail: format!(
                "bundle-local skill name `{}` must match resource id `{local_id}`",
                parsed.name
            ),
        });
    }
    let path = PathBuf::from(format!("bundle:{bundle_id}/{source_path}"));
    let dir = path.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    Ok(SkillCatalogEntry {
        name: parsed.name,
        description: parsed.description,
        content: parsed.content,
        allowed_tools: parsed.allowed_tools,
        model: parsed.model,
        path,
        dir,
    })
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
    for reference in &view.deny {
        let hit = resolve_global_reference(policy, reference, partitions)?;
        match hit.kind {
            "tool" => {
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
    Ok(selected)
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
        AgentRole, BundleIdentity, BundleSource, ModelPolicy, PreparedAgent,
        PreparedBundle, PreparedResource, SourceFile, SpawnLifecycle, prepare_package,
    };
    use hya_proto::{AgentName, ToolName};
    use hya_tool::{
        Action, InvocationPolicy, InvocationRule, Mode, PermissionModel, PermissionPlane,
        PermissionRules, PermissionTarget, Rule, Tool, ToolCtx, ToolError, ToolPermission,
        ToolRegistry,
    };
    use serde_json::{Value, json};
    use std::path::PathBuf;

    /// Test shim: wrap prepared bundles as an [`AgentCatalog`] over the
    /// compiled-in built-in roster, so unit tests keep their existing shape.
    struct TestCatalog;

    impl TestCatalog {
        fn from_prepared(bundles: &[PreparedBundle]) -> Result<AgentCatalog, BundleError> {
            AgentCatalog::new(Arc::new(BundleCatalog::from_prepared(bundles)?))
        }

        fn from_verified_catalogs(
            catalogs: &[&hya_bundle::PreparedCatalog],
        ) -> Result<AgentCatalog, BundleError> {
            AgentCatalog::new(Arc::new(BundleCatalog::from_verified_catalogs(catalogs)?))
        }
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
    ) -> PreparedBundle {
        PreparedBundle {
            format_version: 1,
            identity: BundleIdentity {
                id: bundle_id.to_string(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            digest: "test-only".to_string(),
            agent: agent,
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
            spawn_lifecycle: SpawnLifecycle::Transient,
            resource_view,
            can_spawn: Vec::new(),
            hook_refs: Vec::new(),
        }
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let workdir = PathBuf::from("/tmp/hya-resource-view-pin");
        let binding = registry.bind_turn(&workdir).unwrap();
        let policy = binding.agent_resource_policy_on_plane("pin-agent", AgentToolPlane::Full).unwrap();
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
    fn runtime_source_dispatch_identity_tracks_authoritative_source_semantics() {
        let source_identity =
            |configured_id: &str, declaration_digest: [u8; 32], resource_value: Value| {
                let catalog = Arc::new(
                    TestCatalog::from_prepared(&[bundle_with_agent(
                        "hya/source-identity",
                        agent(
                            "source-identity", ResourceView::default(),
                        ),
                        Vec::new(),
                    )])
                    .unwrap(),
                );
                let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
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
  spawn_lifecycle: transient
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
            };
            (
                TurnBinding {
                    snapshot: Arc::new(snapshot),
                    workdir: PathBuf::from("/tmp/runtime-fingerprint"),
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
  spawn_lifecycle: transient
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
            };
            let (permission, _asks) = PermissionPlane::new_with_policy(
                PermissionRules::default(),
                InvocationPolicy::default(),
            );
            (
                TurnBinding {
                    snapshot: Arc::new(snapshot),
                    workdir,
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
  spawn_lifecycle: transient
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
            let registry = RuntimeRegistry::new(ToolRegistry::builtins(), Arc::new(catalog));
            let mut plugin_resources = BTreeMap::new();
            plugin_resources.insert(
                "config".to_string(),
                nested_value(plugin_resource_marker, reverse_resources),
            );
            let plugin = RuntimeSource::new(
                RuntimeSourceId::new(plugin_kind, plugin_id),
                plugin_digest,
                Arc::new(()),
                vec![RuntimeSourceExport::tool(
                    "probe",
                    "plugin__fixture__probe",
                    vec!["plugin_probe".to_string()],
                    Arc::new(NoopTool::new("plugin__fixture__probe")),
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
                    snapshot: tools,
                    workdir: PathBuf::from("/tmp/runtime-fingerprint-sources"),
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
                    "alias-miss", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
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
                    "alias-denied", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-denied"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("alias-denied", AgentToolPlane::Full).unwrap();
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
                    "alias-qualified", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-qualified"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("alias-qualified", AgentToolPlane::Full).unwrap();
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
                    "alias-collide", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-collide"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("alias-collide", AgentToolPlane::Full).unwrap();
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let workdir = tempfile_skill_workdir("shared", "HARNESS");
        let binding = registry.bind_turn(&workdir).unwrap();
        let policy = binding.agent_resource_policy_on_plane("skill-agent", AgentToolPlane::Full).unwrap();
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
                    "skill-agent", ResourceView {
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
                    aliases: Vec::new(),
                }],
            )])
            .unwrap(),
        );
        let filtered_registry = RuntimeRegistry::new(ToolRegistry::builtins(), filtered_catalog);
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
    fn schema_and_dispatch_name_sets_are_identical() {
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/schema-dispatch",
                agent(
                    "sd-agent", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-schema-dispatch"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("sd-agent", AgentToolPlane::Full).unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        let schema_names = compiled
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(schema_names, compiled.public_tool_names());
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("dynamic_marker"))))
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-none-inline"))
            .unwrap();
        let policy = binding.agent_resource_policy("none-agent").unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        assert!(
            compiled.public_tool_names().is_empty(),
            "an allow list of only bundle-local resources admits no harness tool"
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
            compiled.resolve_tool("read").is_none(),
            "dispatch must not fall back to builtins outside the compiled view"
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
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/qualified-map",
                agent(
                    "q-agent", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-qualified-map"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("q-agent", AgentToolPlane::Full).unwrap();
        let compiled = binding.compile_agent_resources(&policy).unwrap();
        let schema_names = compiled
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(schema_names, compiled.public_tool_names());
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
                        "allow-mcp", ResourceView {
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
                        "deny-mcp", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
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
            .compile_agent_resources(&binding.agent_resource_policy_on_plane("full-mcp", AgentToolPlane::Full).unwrap())
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
            .compile_agent_resources(&binding.agent_resource_policy_on_plane("allow-mcp", AgentToolPlane::Full).unwrap())
            .unwrap();
        assert!(allowed.resolve_tool("mcp__fixture__ping").is_some());
        assert!(allowed.resolve_tool("read").is_none());

        let denied = binding
            .compile_agent_resources(&binding.agent_resource_policy_on_plane("deny-mcp", AgentToolPlane::Full).unwrap())
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
            .compile_agent_resources(&binding.agent_resource_policy_on_plane("full-mcp", AgentToolPlane::Full).unwrap())
            .unwrap();
        assert!(
            pinned.resolve_tool("mcp__fixture__ping").is_some(),
            "old TurnBinding must pin the prior MCP view"
        );
        let fresh = registry.bind_turn(workdir).unwrap();
        let after = fresh
            .compile_agent_resources(&fresh.agent_resource_policy_on_plane("full-mcp", AgentToolPlane::Full).unwrap())
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
            aliases: Vec::new(),
        };
        let wrong_kind = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/global-ref",
                agent(
                    "wrong-kind", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), wrong_kind);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-global-wrong-kind"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("wrong-kind", AgentToolPlane::Full).unwrap();
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
                    "ambiguous", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), ambiguous);
        // Register a harness tool whose short name collides with the local skill.
        registry
            .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("shared"))))
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-global-ambiguous"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("ambiguous", AgentToolPlane::Full).unwrap();
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
                    "alias-self", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-self"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("alias-self", AgentToolPlane::Full).unwrap();
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
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/cross-kind-ok",
                agent(
                    "cross-ok", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-cross-kind-ok"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("cross-ok", AgentToolPlane::Full).unwrap();
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
                    "collide", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
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
        let policy = binding.agent_resource_policy_on_plane("collide", AgentToolPlane::Full).unwrap();
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
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-ambiguous",
                agent(
                    "alias-amb", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("shared"))))
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-ambiguous"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("alias-amb", AgentToolPlane::Full).unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::NamespaceCollision { name, .. }) => {
                assert_eq!(name, "shared");
            }
            Ok(_) => panic!("cross-kind ambiguous alias target must typed-reject"),
            Err(other) => panic!("expected namespace collision, got {other:?}"),
        }
    }

    #[test]
    fn both_planes_preserve_builtin_alias_in_public_names() {
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
            let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
            let binding = registry.bind_turn(Path::new(workdir)).unwrap();
            let policy = binding
                .agent_resource_policy_on_plane(agent_id, plane)
                .unwrap();
            let compiled = binding.compile_agent_resources(&policy).unwrap();
            assert!(
                compiled.resolve_tool("apply_patch").is_some(),
                "{agent_id}: canonical apply_patch must remain public"
            );
            assert!(
                compiled.resolve_tool("patch").is_some(),
                "{agent_id}: builtin alias `patch` must enter compiled public names"
            );
            let schema_names = compiled
                .tool_schemas()
                .into_iter()
                .map(|schema| schema.name.as_str().to_string())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                schema_names,
                compiled.public_tool_names(),
                "{agent_id}: schema and dispatch sets must stay identical with aliases"
            );
            assert!(schema_names.contains("patch"));
            assert!(schema_names.contains("apply_patch"));
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
                    "suppress-agent", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-view-alias-suppress"))
            .unwrap();
        let compiled = binding
            .compile_agent_resources(&binding.agent_resource_policy_on_plane("suppress-agent", AgentToolPlane::Full).unwrap())
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
        assert_eq!(
            schema_names,
            compiled.public_tool_names(),
            "schema and dispatch sets must remain identical"
        );
        assert!(schema_names.contains("applier"));
        assert!(schema_names.contains("harness:tool/apply_patch"));
        assert!(!schema_names.contains("apply_patch"));
        assert!(!schema_names.contains("patch"));
    }

    #[test]
    fn explicit_view_alias_collides_with_candidate_alias_even_same_target() {
        // Explicit view alias named `patch` targeting harness:tool/apply_patch
        // collides with the existing candidate alias of that same tool.
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/view-alias-candidate-collide",
                agent(
                    "collide-agent", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-view-alias-candidate-collide"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("collide-agent", AgentToolPlane::Full).unwrap();
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
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/mcp-skill-ok",
                agent(
                    "mcp-skill-ok", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
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
            .compile_agent_resources(&binding.agent_resource_policy_on_plane("mcp-skill-ok", AgentToolPlane::Full).unwrap())
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
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
            .compile_agent_resources(&binding.agent_resource_policy_on_plane("mcp-alias", AgentToolPlane::Full).unwrap())
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
        assert_eq!(schema_names, compiled.public_tool_names());
        assert!(schema_names.contains("pingy"));
    }

    #[test]
    fn prepared_skill_alias_is_public_spelling_for_skill_plane() {
        let local = PreparedResource {
            local_id: "docs".to_string(),
            stable_id: "bundle:hya/skill-alias/skill/docs".to_string(),
            source_path: "resources/skills/docs.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("docs", "DOCS_BODY"),
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
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
                        "allow-alias", ResourceView {
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
                        "deny-alias", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
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
                    "filtered-stable", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-filtered-stable"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("filtered-stable", AgentToolPlane::Full).unwrap();
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
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                "hya/facade-alias",
                agent(
                    "facade-alias", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-facade-alias"))
            .unwrap();
        let compiled = binding
            .compile_agent_resources(&binding.agent_resource_policy_on_plane("facade-alias", AgentToolPlane::Full).unwrap())
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
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
                    "no-facade", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let workdir = tempfile_skill_workdir("workdir-only", "MUST_NOT_INLINE");
        let binding = registry.bind_turn(&workdir).unwrap();
        let policy = binding.agent_resource_policy_on_plane("no-facade", AgentToolPlane::Full).unwrap();
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
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            TestCatalog::from_prepared(&[bundle_with_agent(
                bundle_id,
                agent(
                    "nested-agent", ResourceView {
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
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
    fn bundle_sidecar_tool_binding_owns_short_name_and_shares_schema_dispatch_map() {
        let bundle_id = "hya/sidecar-map";
        let mut bundle = bundle_with_agent(
            bundle_id,
            agent(
                "sidecar-agent", ResourceView::default(),
            ),
            Vec::new(),
        );
        bundle.tools.push(PreparedResource {
            local_id: "echo".to_string(),
            stable_id: format!("bundle:{bundle_id}/tool/echo"),
            source_path: "tools/echo.js".to_string(),
            digest: "test-only-digest".to_string(),
            content: "export default {}".to_string(),
            aliases: Vec::new(),
        });
        let catalog = Arc::new(TestCatalog::from_prepared(&[bundle]).unwrap());

        let tools = ToolRegistry::builtins();
        tools
            .register_with_permission(Arc::new(NoopTool::new("echo")), ToolPermission::Tool)
            .unwrap();
        let registry = RuntimeRegistry::new(tools, catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-sidecar-tool-map"))
            .unwrap();
        let policy = binding.agent_resource_policy_on_plane("sidecar-agent", AgentToolPlane::Full).unwrap();
        let sidecar_tool = ResolvedTool {
            tool: Arc::new(NoopTool::new(format!("bundle:{bundle_id}/tool/echo"))),
            permission: ToolPermission::Tool,
        };
        let compiled = binding
            .compile_agent_resources_with_sidecar_tools(&policy, &[sidecar_tool])
            .unwrap();
        let schema_names = compiled
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(schema_names, compiled.public_tool_names());

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
            "alpha-agent", ResourceView {
                allow: vec![alpha_tool_id.clone()],
                deny: Vec::new(),
                aliases: BTreeMap::new(),
                namespace: None,
            },
        );
        alpha.hook_refs = vec![alpha_hook_id.clone()];

        let mut beta = agent(
            "beta-agent", ResourceView {
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
            aliases: Vec::new(),
        }];
        alpha_bundle.hooks = vec![PreparedResource {
            local_id: "event".to_string(),
            stable_id: alpha_hook_id.clone(),
            source_path: "extensions/event.js".to_string(),
            digest: "alpha-hook".to_string(),
            content: "export default {}".to_string(),
            aliases: Vec::new(),
        }];

        let mut beta_bundle = bundle_with_agent(beta_bundle_id, beta, Vec::new());
        beta_bundle.tools = vec![PreparedResource {
            local_id: "beta".to_string(),
            stable_id: beta_tool_id.clone(),
            source_path: "extensions/beta.js".to_string(),
            digest: "beta-tool".to_string(),
            content: "export default {}".to_string(),
            aliases: Vec::new(),
        }];
        beta_bundle.hooks = vec![PreparedResource {
            local_id: "tool.execute.before".to_string(),
            stable_id: beta_hook_id.clone(),
            source_path: "extensions/before.js".to_string(),
            digest: "beta-hook".to_string(),
            content: "export default {}".to_string(),
            aliases: Vec::new(),
        }];

        let catalog =
            Arc::new(TestCatalog::from_prepared(&[alpha_bundle, beta_bundle]).unwrap());
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
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

        let alpha_names = alpha_compiled.public_tool_names();
        let beta_names = beta_compiled.public_tool_names();
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
}
