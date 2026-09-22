use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use crate::skill_catalog::{
    SkillCatalogOrigin, discover_skills_from_dirs, discover_skills_with_builtins,
};

/// Session-scoped skill catalog access for the `skill` tool.
#[derive(Clone)]
pub struct SkillPlane {
    roots: SkillRoots,
}

#[derive(Clone)]
enum SkillRoots {
    DefaultForWorkdir,
    ExplicitDirs(Arc<Vec<PathBuf>>),
    Snapshot(Arc<Vec<crate::SkillCatalogEntry>>),
}

impl Default for SkillPlane {
    fn default() -> Self {
        Self {
            roots: SkillRoots::DefaultForWorkdir,
        }
    }
}

impl SkillPlane {
    /// Resolve skills only from the given directory roots.
    #[must_use]
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self {
            roots: SkillRoots::ExplicitDirs(Arc::new(dirs)),
        }
    }

    /// Build a plane over the exact immutable catalog captured by a turn.
    #[must_use]
    pub fn from_snapshot(skills: Arc<Vec<crate::SkillCatalogEntry>>) -> Self {
        Self {
            roots: SkillRoots::Snapshot(skills),
        }
    }

    /// Skill body for `skill://<name>`, without the `skill` tool's framing.
    ///
    /// Returns `None` for an unknown name so the router can report it as a
    /// missing handle rather than leaking a tool-level error type.
    pub(crate) fn body(&self, workdir: &Path, name: &str) -> Option<String> {
        self.require(workdir, name).ok().map(|info| info.content)
    }

    /// Resolve a named skill against this plane's captured catalog.
    ///
    /// # Errors
    /// Returns [`SkillError::NotFound`] when the name is unavailable.
    pub fn require(&self, workdir: &Path, name: &str) -> Result<SkillInfo, SkillError> {
        let skill = match &self.roots {
            SkillRoots::DefaultForWorkdir => discover_skills_with_builtins(workdir)
                .into_iter()
                .find(|skill| skill.name == name),
            SkillRoots::ExplicitDirs(dirs) => discover_skills_from_dirs(dirs)
                .into_iter()
                .find(|skill| skill.name == name),
            SkillRoots::Snapshot(skills) => skills.iter().find(|skill| skill.name == name).cloned(),
        };
        let Some(skill) = skill else {
            return Err(SkillError::NotFound(name.to_string()));
        };
        let dir = match skill.origin {
            SkillCatalogOrigin::Embedded => None,
            SkillCatalogOrigin::Filesystem | SkillCatalogOrigin::Virtual => {
                Some(canonical_or_self(&skill.dir))
            }
        };
        Ok(SkillInfo {
            name: skill.name,
            dir,
            origin: skill.origin,
            content: skill.content,
        })
    }
}

/// Failure to resolve a skill from the captured catalog.
#[derive(Debug, Error)]
pub enum SkillError {
    /// No skill with the requested name exists in the captured catalog.
    #[error("skill not found: {0}")]
    NotFound(String),
}

/// Resolved skill body and provenance from a session's captured catalog.
pub struct SkillInfo {
    /// Canonical skill name.
    pub name: String,
    /// Filesystem base when the skill is not embedded.
    pub dir: Option<PathBuf>,
    /// Source class used to frame the tool result.
    pub origin: SkillCatalogOrigin,
    /// Markdown body.
    pub content: String,
}

fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
