use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use hya_proto::{ConfigGeneration, ToolSchema};
use hya_tool::{
    DuplicateName, ResolvedTool, SkillCatalogEntry, SkillPlane, Tool, ToolPermission, ToolRegistry,
    ToolRegistrySnapshot, discover_skills,
};
use thiserror::Error;

/// A complete immutable configuration view. Turns retain its `Arc` for their
/// whole lifetime, so publication cannot alter an in-flight lookup.
struct RuntimeSnapshot {
    generation: ConfigGeneration,
    tools: ToolRegistrySnapshot,
    skills: BTreeMap<PathBuf, Arc<Vec<SkillCatalogEntry>>>,
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
        self.tools.logically_matches(&snapshot.tools) && self.skills == snapshot.skills
    }
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
