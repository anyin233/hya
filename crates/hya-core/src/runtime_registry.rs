use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use hya_bundle::{
    BundleCatalog, BundleError, ExportKind, HarnessAccess, PreparedAgent, ResourceView,
};
use hya_proto::{ConfigGeneration, ToolName, ToolSchema};
use hya_tool::{
    DuplicateName, ResolvedTool, SkillCatalogEntry, SkillPlane, Tool, ToolPermission, ToolRegistry,
    ToolRegistrySnapshot, discover_skills, parse_skill,
};
use serde_json::Value;
use thiserror::Error;

/// A complete immutable configuration view. Turns retain its `Arc` for their
/// whole lifetime, so publication cannot alter an in-flight lookup.
struct RuntimeSnapshot {
    generation: ConfigGeneration,
    catalog: Arc<BundleCatalog>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeSourceKind {
    Mcp,
    Plugin,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeSourceId {
    kind: RuntimeSourceKind,
    configured_id: String,
}

pub trait RuntimeSourceOwner: Send + Sync {}

impl<T: Send + Sync> RuntimeSourceOwner for T {}

#[derive(Clone)]
pub struct RuntimeSourceExport {
    declared_id: String,
    canonical_name: String,
    aliases: Vec<String>,
    tool: Arc<dyn Tool>,
    permission: ToolPermission,
}

#[derive(Clone)]
pub struct RuntimeSource {
    id: RuntimeSourceId,
    declaration_digest: [u8; 32],
    owner: Arc<dyn RuntimeSourceOwner>,
    exports: Vec<RuntimeSourceExport>,
    resources: Arc<BTreeMap<String, Value>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSourceManifest {
    pub id: RuntimeSourceId,
    pub declaration_digest: [u8; 32],
    pub exports: Vec<String>,
    pub resources: Arc<BTreeMap<String, Value>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeEffectiveManifest {
    pub generation: ConfigGeneration,
    pub sources: BTreeMap<RuntimeSourceId, RuntimeSourceManifest>,
}

/// One admitted turn's immutable runtime binding.
#[derive(Clone)]
pub struct TurnBinding {
    snapshot: Arc<RuntimeSnapshot>,
    workdir: PathBuf,
}

/// Catalog-derived policy retained only for one in-process agent execution.
/// It contains no agent identity and is never persisted or exposed on the wire.
#[derive(Clone, Debug)]
pub struct AgentResourcePolicy {
    bundle_id: String,
    harness_access: HarnessAccess,
    resource_view: ResourceView,
}

/// Immutable per-turn/child resource map compiled once from a retained
/// [`TurnBinding`] and bound agent policy. Schema visibility, skill exposure,
/// and dispatch share this map; there is no registry fallback.
pub(crate) struct CompiledResourceView {
    tools: BTreeMap<String, ResolvedTool>,
    schemas: Vec<ToolSchema>,
    skills: Arc<Vec<SkillCatalogEntry>>,
    /// Whether the selected view includes the canonical harness skill facade
    /// tool (regardless of any public alias spelling for that tool).
    skill_facade_selected: bool,
}

#[derive(Debug, Error)]
pub enum RuntimeRefreshError {
    #[error(transparent)]
    DuplicateTool(#[from] DuplicateName),
    #[error("configuration generation exhausted")]
    GenerationExhausted,
    #[error("invalid runtime candidate: {0}")]
    InvalidCandidate(String),
}

impl RuntimeRegistry {
    #[must_use]
    pub fn new(tools: ToolRegistry, catalog: Arc<BundleCatalog>) -> Self {
        Self::from_snapshot(tools.snapshot(), catalog)
    }

    #[must_use]
    pub fn from_snapshot(tools: ToolRegistrySnapshot, catalog: Arc<BundleCatalog>) -> Self {
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
        catalog: Arc<BundleCatalog>,
    ) -> Result<ConfigGeneration, RuntimeRefreshError> {
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = self.active();
        if current.catalog.bundles() == catalog.bundles() {
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
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.active().tools.schemas()
    }

    #[must_use]
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

    pub fn register_tool(&mut self, tool: Arc<dyn Tool>) -> Result<(), RuntimeRefreshError> {
        self.register_tool_with_permission(tool, ToolPermission::Tool)
    }

    pub fn register_tool_with_permission(
        &mut self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
    ) -> Result<(), RuntimeRefreshError> {
        self.tools.register_with_permission(tool, permission)?;
        Ok(())
    }

    pub fn remove_tool(&mut self, name: &str) {
        if self.tools.resolve(name).is_some() {
            self.tools.remove(name);
        }
    }

    pub fn refresh_skills(&mut self, workdir: &Path) {
        self.replace_skills(workdir, discover_skills(workdir));
    }

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
                self.tools.register_with_permission_and_aliases(
                    export.tool.clone(),
                    export.permission,
                    &export.aliases,
                )?;
            }
            self.sources.insert(source.id.clone(), source);
        }
        Ok(())
    }

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
    pub fn new(kind: RuntimeSourceKind, configured_id: impl Into<String>) -> Self {
        Self {
            kind,
            configured_id: configured_id.into(),
        }
    }

    #[must_use]
    pub fn mcp(configured_id: impl Into<String>) -> Self {
        Self::new(RuntimeSourceKind::Mcp, configured_id)
    }

    #[must_use]
    pub fn plugin(configured_id: impl Into<String>) -> Self {
        Self::new(RuntimeSourceKind::Plugin, configured_id)
    }

    #[must_use]
    pub fn kind(&self) -> RuntimeSourceKind {
        self.kind
    }

    #[must_use]
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
    pub fn with_resources(mut self, resources: BTreeMap<String, Value>) -> Self {
        self.resources = Arc::new(resources);
        self
    }

    #[must_use]
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
    #[must_use]
    pub fn generation(&self) -> ConfigGeneration {
        self.snapshot.generation
    }

    #[must_use]
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    #[must_use]
    pub fn agent_catalog(&self) -> &BundleCatalog {
        &self.snapshot.catalog
    }

    #[must_use]
    pub fn resolve_agent(&self, stable_id: &str) -> Option<&PreparedAgent> {
        self.snapshot.catalog.resolve_agent(stable_id)
    }

    pub fn resolve_requested_agent(
        &self,
        requested: Option<&str>,
    ) -> Result<&PreparedAgent, BundleError> {
        let stable_id = requested.unwrap_or("general");
        self.resolve_agent(stable_id)
            .ok_or_else(|| BundleError::UnknownAgentId {
                agent_id: stable_id.to_string(),
            })
    }

    pub fn resolve_spawn(
        &self,
        caller: &str,
        requested: &str,
    ) -> Result<&PreparedAgent, BundleError> {
        self.snapshot.catalog.resolve_spawn(caller, requested)
    }

    pub fn spawnable_agents(&self, caller: &str) -> Result<Vec<&PreparedAgent>, BundleError> {
        self.snapshot.catalog.spawnable_agents(caller)
    }

    pub fn agent_resource_policy(
        &self,
        stable_id: &str,
    ) -> Result<AgentResourcePolicy, BundleError> {
        let (bundle_id, agent) = self
            .snapshot
            .catalog
            .resolve_agent_entry(stable_id)
            .ok_or_else(|| BundleError::UnknownAgentId {
                agent_id: stable_id.to_string(),
            })?;
        Ok(AgentResourcePolicy {
            bundle_id: bundle_id.to_string(),
            harness_access: agent.harness_access,
            resource_view: agent.resource_view.clone(),
        })
    }

    pub(crate) fn compile_agent_resources(
        &self,
        policy: &AgentResourcePolicy,
    ) -> Result<Arc<CompiledResourceView>, BundleError> {
        let view = &policy.resource_view;
        let bundle_id = policy.bundle_id.as_str();
        let namespace = view.namespace.as_deref().unwrap_or(bundle_id);

        let mut tool_candidates = BTreeMap::new();
        collect_bundle_tool_candidates(
            self.snapshot.catalog.as_ref(),
            bundle_id,
            &mut tool_candidates,
        )?;
        collect_harness_tool_candidates(
            policy.harness_access,
            &self.snapshot.basic_tools,
            &self.snapshot.tools,
            &self.snapshot.sources,
            &mut tool_candidates,
        );

        let mut skill_candidates = BTreeMap::new();
        collect_bundle_skill_candidates(
            self.snapshot.catalog.as_ref(),
            bundle_id,
            &mut skill_candidates,
        )?;
        collect_harness_skill_candidates(
            policy.harness_access,
            self.skills(),
            &mut skill_candidates,
        );

        let mut mcp_candidates = BTreeMap::new();
        collect_bundle_mcp_candidates(
            self.snapshot.catalog.as_ref(),
            bundle_id,
            &mut mcp_candidates,
        )?;
        collect_harness_mcp_candidates(
            policy.harness_access,
            &self.snapshot.sources,
            &mut mcp_candidates,
        );

        let partitions = CandidatePartitions {
            tool: tool_candidates,
            skill: skill_candidates,
            mcp: mcp_candidates,
        };
        let selected = select_candidates_globally(bundle_id, &partitions, view)?;

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

        let view_aliases = resolve_view_aliases(bundle_id, &partitions, &selected, view)?;
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
                    return Err(BundleError::UnsupportedBundleFeature {
                        bundle_id: bundle_id.to_string(),
                        feature: "resources.tools".to_string(),
                    });
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

        Ok(Arc::new(CompiledResourceView {
            tools,
            schemas,
            skills: Arc::new(skills),
            skill_facade_selected,
        }))
    }

    #[must_use]
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.snapshot.tools.schemas()
    }

    #[must_use]
    pub fn resolve_tool(&self, name: &str) -> Option<ResolvedTool> {
        self.snapshot.tools.resolve(name)
    }

    #[must_use]
    pub fn skills(&self) -> &[SkillCatalogEntry] {
        self.snapshot
            .skills
            .get(&self.workdir)
            .map_or(&[], |skills| skills.as_slice())
    }

    #[must_use]
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
    access: HarnessAccess,
    basic_tools: &ToolRegistrySnapshot,
    full_tools: &ToolRegistrySnapshot,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    let selected = match access {
        HarnessAccess::None => return,
        HarnessAccess::Basic => basic_tools,
        HarnessAccess::Full => full_tools,
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
    access: HarnessAccess,
    harness_skills: &[SkillCatalogEntry],
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    if access != HarnessAccess::Full {
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
    access: HarnessAccess,
    sources: &BTreeMap<RuntimeSourceId, RuntimeSource>,
    out: &mut BTreeMap<String, ResourceCandidate>,
) {
    if access != HarnessAccess::Full {
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
    bundle_id: &str,
    partitions: &CandidatePartitions,
    view: &ResourceView,
) -> Result<SelectedIds, BundleError> {
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
            let hit = resolve_global_reference(bundle_id, reference, partitions)?;
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
        let hit = resolve_global_reference(bundle_id, reference, partitions)?;
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
    bundle_id: &str,
    reference: &str,
    partitions: &CandidatePartitions,
) -> Result<ResolvedReference, BundleError> {
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
    bundle_id: &str,
    partitions: &CandidatePartitions,
    selected: &SelectedIds,
    view: &ResourceView,
) -> Result<Vec<(String, &'static str, String)>, BundleError> {
    let mut out = Vec::new();
    for (alias, target) in &view.aliases {
        let hit = resolve_global_reference(bundle_id, target, partitions)?;
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
        AgentRole, BundleIdentity, BundleOrigin, ModelPolicy, PreparedAgent, PreparedBundle,
        PreparedResource, SpawnLifecycle,
    };
    use hya_proto::{AgentName, ToolName};
    use hya_tool::{Tool, ToolCtx, ToolError, ToolRegistry};
    use serde_json::{Value, json};
    use std::path::PathBuf;

    struct NoopTool {
        name: String,
    }

    impl NoopTool {
        fn new(name: impl Into<String>) -> Self {
            Self { name: name.into() }
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
            origin: BundleOrigin::Builtin,
            immutable: true,
            digest: "test-only".to_string(),
            agents: vec![agent],
            tools: Vec::new(),
            skills,
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
        }
    }

    fn agent(
        stable_id: &str,
        harness_access: HarnessAccess,
        resource_view: ResourceView,
    ) -> PreparedAgent {
        PreparedAgent {
            local_id: stable_id.to_string(),
            stable_id: AgentName::new(stable_id),
            description: None,
            role: AgentRole::Main,
            color: None,
            prompt: Some("prompt".to_string()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            spawn_lifecycle: SpawnLifecycle::Transient,
            harness_access,
            resource_view,
            can_spawn: Vec::new(),
            hook_refs: Vec::new(),
        }
    }

    #[test]
    fn old_turn_binding_pins_tools_across_later_publication() {
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/pin",
                agent("pin-agent", HarnessAccess::Full, ResourceView::default()),
                Vec::new(),
            )])
            .unwrap(),
        );
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let workdir = PathBuf::from("/tmp/hya-resource-view-pin");
        let binding = registry.bind_turn(&workdir).unwrap();
        let policy = binding.agent_resource_policy("pin-agent").unwrap();
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
    fn missing_and_filtered_alias_targets_fail_typed() {
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-miss",
                agent(
                    "alias-miss",
                    HarnessAccess::Basic,
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-denied",
                agent(
                    "alias-denied",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-denied"))
            .unwrap();
        let policy = binding.agent_resource_policy("alias-denied").unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::UnknownResourceReference { .. }) => {}
            Ok(_) => panic!("expected denied alias target to fail"),
            Err(other) => panic!("expected denied alias target to fail, got {other:?}"),
        }
    }

    #[test]
    fn alias_cannot_override_qualified_stable_name() {
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-qualified",
                agent(
                    "alias-qualified",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-qualified"))
            .unwrap();
        let policy = binding.agent_resource_policy("alias-qualified").unwrap();
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-collide",
                agent(
                    "alias-collide",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-collide"))
            .unwrap();
        let policy = binding.agent_resource_policy("alias-collide").unwrap();
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/skill-collision",
                agent("skill-agent", HarnessAccess::Full, ResourceView::default()),
                vec![local],
            )])
            .unwrap(),
        );
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let workdir = tempfile_skill_workdir("shared", "HARNESS");
        let binding = registry.bind_turn(&workdir).unwrap();
        let policy = binding.agent_resource_policy("skill-agent").unwrap();
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/skill-collision",
                agent(
                    "skill-agent",
                    HarnessAccess::Full,
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
                    aliases: Vec::new(),
                }],
            )])
            .unwrap(),
        );
        let filtered_registry = RuntimeRegistry::new(ToolRegistry::builtins(), filtered_catalog);
        let filtered_binding = filtered_registry.bind_turn(&workdir).unwrap();
        let filtered_policy = filtered_binding
            .agent_resource_policy("skill-agent")
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/schema-dispatch",
                agent(
                    "sd-agent",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-schema-dispatch"))
            .unwrap();
        let policy = binding.agent_resource_policy("sd-agent").unwrap();
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
    fn none_access_has_no_skill_facade_and_inlines_local_static_skill() {
        let local = PreparedResource {
            local_id: "bundle-skill".to_string(),
            stable_id: "bundle:hya/none-inline/skill/bundle-skill".to_string(),
            source_path: "resources/skills/bundle-skill.md".to_string(),
            digest: "test-only".to_string(),
            content: skill_md("bundle-skill", "INLINE_BODY_MARKER"),
            aliases: Vec::new(),
        };
        let catalog = Arc::new(
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/none-inline",
                agent("none-agent", HarnessAccess::None, ResourceView::default()),
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
            "None must expose no harness skill facade or other harness tools"
        );
        assert!(
            compiled.resolve_tool("skill").is_none(),
            "None must not insert the harness skill tool"
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/qualified-map",
                agent(
                    "q-agent",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-qualified-map"))
            .unwrap();
        let policy = binding.agent_resource_policy("q-agent").unwrap();
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
            BundleCatalog::from_prepared(&[
                bundle_with_agent(
                    "hya/mcp-full",
                    agent("full-mcp", HarnessAccess::Full, ResourceView::default()),
                    Vec::new(),
                ),
                bundle_with_agent(
                    "hya/mcp-basic",
                    agent("basic-mcp", HarnessAccess::Basic, ResourceView::default()),
                    Vec::new(),
                ),
                bundle_with_agent(
                    "hya/mcp-allow",
                    agent(
                        "allow-mcp",
                        HarnessAccess::Full,
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
                        HarnessAccess::Full,
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
            .compile_agent_resources(&binding.agent_resource_policy("full-mcp").unwrap())
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
            .compile_agent_resources(&binding.agent_resource_policy("allow-mcp").unwrap())
            .unwrap();
        assert!(allowed.resolve_tool("mcp__fixture__ping").is_some());
        assert!(allowed.resolve_tool("read").is_none());

        let denied = binding
            .compile_agent_resources(&binding.agent_resource_policy("deny-mcp").unwrap())
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
            .compile_agent_resources(&binding.agent_resource_policy("full-mcp").unwrap())
            .unwrap();
        assert!(
            pinned.resolve_tool("mcp__fixture__ping").is_some(),
            "old TurnBinding must pin the prior MCP view"
        );
        let fresh = registry.bind_turn(workdir).unwrap();
        let after = fresh
            .compile_agent_resources(&fresh.agent_resource_policy("full-mcp").unwrap())
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/global-ref",
                agent(
                    "wrong-kind",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), wrong_kind);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-global-wrong-kind"))
            .unwrap();
        let policy = binding.agent_resource_policy("wrong-kind").unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::UnknownResourceReference { reference, .. }) => {
                assert_eq!(reference, "harness:skill/read");
            }
            Ok(_) => panic!("wrong-kind prefix must typed-reject"),
            Err(other) => panic!("expected unknown resource reference, got {other:?}"),
        }

        let ambiguous = Arc::new(
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/global-ref",
                agent(
                    "ambiguous",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), ambiguous);
        // Register a harness tool whose short name collides with the local skill.
        registry
            .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("shared"))))
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-global-ambiguous"))
            .unwrap();
        let policy = binding.agent_resource_policy("ambiguous").unwrap();
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-self",
                agent(
                    "alias-self",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-self"))
            .unwrap();
        let policy = binding.agent_resource_policy("alias-self").unwrap();
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/cross-kind-ok",
                agent(
                    "cross-ok",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-cross-kind-ok"))
            .unwrap();
        let policy = binding.agent_resource_policy("cross-ok").unwrap();
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/tool-mcp-collide",
                agent(
                    "collide",
                    HarnessAccess::Full,
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
        let policy = binding.agent_resource_policy("collide").unwrap();
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/alias-ambiguous",
                agent(
                    "alias-amb",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        registry
            .refresh(|candidate| candidate.register_tool(Arc::new(NoopTool::new("shared"))))
            .unwrap();
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-alias-ambiguous"))
            .unwrap();
        let policy = binding.agent_resource_policy("alias-amb").unwrap();
        match binding.compile_agent_resources(&policy) {
            Err(BundleError::NamespaceCollision { name, .. }) => {
                assert_eq!(name, "shared");
            }
            Ok(_) => panic!("cross-kind ambiguous alias target must typed-reject"),
            Err(other) => panic!("expected namespace collision, got {other:?}"),
        }
    }

    #[test]
    fn full_and_basic_preserve_builtin_alias_in_public_names() {
        for (agent_id, access, workdir) in [
            (
                "full-alias",
                HarnessAccess::Full,
                "/tmp/hya-builtin-alias-full",
            ),
            (
                "basic-alias",
                HarnessAccess::Basic,
                "/tmp/hya-builtin-alias-basic",
            ),
        ] {
            let catalog = Arc::new(
                BundleCatalog::from_prepared(&[bundle_with_agent(
                    "hya/builtin-alias",
                    agent(agent_id, access, ResourceView::default()),
                    Vec::new(),
                )])
                .unwrap(),
            );
            let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
            let binding = registry.bind_turn(Path::new(workdir)).unwrap();
            let policy = binding.agent_resource_policy(agent_id).unwrap();
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/view-alias-suppress",
                agent(
                    "suppress-agent",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-view-alias-suppress"))
            .unwrap();
        let compiled = binding
            .compile_agent_resources(&binding.agent_resource_policy("suppress-agent").unwrap())
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/view-alias-candidate-collide",
                agent(
                    "collide-agent",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-view-alias-candidate-collide"))
            .unwrap();
        let policy = binding.agent_resource_policy("collide-agent").unwrap();
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/mcp-skill-ok",
                agent(
                    "mcp-skill-ok",
                    HarnessAccess::Full,
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
            .compile_agent_resources(&binding.agent_resource_policy("mcp-skill-ok").unwrap())
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/mcp-alias",
                agent("mcp-alias", HarnessAccess::Full, ResourceView::default()),
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
            .compile_agent_resources(&binding.agent_resource_policy("mcp-alias").unwrap())
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/skill-alias",
                agent("skill-alias", HarnessAccess::None, ResourceView::default()),
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
            BundleCatalog::from_prepared(&[
                bundle_with_agent(
                    "hya/allow-alias",
                    agent(
                        "allow-alias",
                        HarnessAccess::Full,
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
                        HarnessAccess::Full,
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/filtered-stable",
                agent(
                    "filtered-stable",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-filtered-stable"))
            .unwrap();
        let policy = binding.agent_resource_policy("filtered-stable").unwrap();
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/facade-alias",
                agent(
                    "facade-alias",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let binding = registry
            .bind_turn(Path::new("/tmp/hya-facade-alias"))
            .unwrap();
        let compiled = binding
            .compile_agent_resources(&binding.agent_resource_policy("facade-alias").unwrap())
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/spellings",
                agent("spellings", HarnessAccess::None, ResourceView::default()),
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
            .expect("none access inlines selected local static skills");
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                "hya/no-facade",
                agent(
                    "no-facade",
                    HarnessAccess::Full,
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
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), catalog);
        let workdir = tempfile_skill_workdir("workdir-only", "MUST_NOT_INLINE");
        let binding = registry.bind_turn(&workdir).unwrap();
        let policy = binding.agent_resource_policy("no-facade").unwrap();
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
            BundleCatalog::from_prepared(&[bundle_with_agent(
                bundle_id,
                agent(
                    "nested-agent",
                    HarnessAccess::None,
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
