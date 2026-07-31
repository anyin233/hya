use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use hya_proto::{ConfigGeneration, ToolSchema};
use hya_tool::{
    DuplicateName, ResolvedTool, SkillCatalogEntry, SkillPlane, Tool, ToolPermission, ToolRegistry,
    ToolRegistrySnapshot, discover_skills,
};
use serde_json::Value;
use thiserror::Error;

/// A complete immutable configuration view. Turns retain its `Arc` for their
/// whole lifetime, so publication cannot alter an in-flight lookup.
struct RuntimeSnapshot {
    generation: ConfigGeneration,
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
    pub fn new(tools: ToolRegistry) -> Self {
        Self::from_snapshot(tools.snapshot())
    }

    #[must_use]
    pub fn from_snapshot(tools: ToolRegistrySnapshot) -> Self {
        Self {
            publication: Mutex::new(()),
            active: RwLock::new(Arc::new(RuntimeSnapshot {
                generation: ConfigGeneration::INITIAL,
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
