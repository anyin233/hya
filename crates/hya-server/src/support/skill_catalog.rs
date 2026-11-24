use hya_tool::{
    SkillCatalogOrigin, discover_skills_for_roots_with_builtins, discover_user_skills_with_builtins,
};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::support::catalog_place::CatalogPlace;

#[derive(Clone, Serialize)]
pub(crate) struct SkillInfo {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) location: String,
    /// The skill's own directory, the base of its relative references;
    /// `None` for a built-in skill (no filesystem base).
    #[serde(skip)]
    pub(crate) dir: Option<String>,
    /// The catalog path (`embedded:hya/core-skills/skill/<name>/SKILL.md`
    /// for a built-in skill), named by an expanded `/skill:<name>` block.
    #[serde(skip)]
    pub(crate) path: String,
    pub(crate) content: String,
    /// `builtin`, `project`, `user`, or `bundle` (`SkillSummary.source`).
    pub(crate) source: &'static str,
}

/// Where a discovered skill comes from: compiled in (`builtin`), a runtime
/// resource (`bundle`), a file under the directory or a Project root
/// (`project`, checked first, since a project usually lives under `$HOME`),
/// or anywhere else, such as `~/.codex/skills` (`user`).
pub(crate) fn skill_source(
    origin: SkillCatalogOrigin,
    path: &Path,
    workdir: Option<&Path>,
    roots: &[PathBuf],
) -> &'static str {
    match origin {
        SkillCatalogOrigin::Embedded => "builtin",
        SkillCatalogOrigin::Virtual => "bundle",
        SkillCatalogOrigin::Filesystem => {
            let project = workdir
                .into_iter()
                .chain(roots.iter().map(PathBuf::as_path))
                .any(|root| path.starts_with(root));
            if project { "project" } else { "user" }
        }
    }
}

/// Skills visible at `place`: its directory first, then each Project root
/// in order (first wins); the global view lists user skills and builtins.
pub(crate) fn list(place: &CatalogPlace) -> Vec<SkillInfo> {
    place
        .workdir()
        .map_or_else(discover_user_skills_with_builtins, |workdir| {
            discover_skills_for_roots_with_builtins(workdir, place.roots())
        })
        .into_iter()
        .map(|skill| {
            let location = match skill.origin {
                SkillCatalogOrigin::Embedded => "<built-in>".to_string(),
                SkillCatalogOrigin::Filesystem | SkillCatalogOrigin::Virtual => {
                    skill.path.to_string_lossy().into_owned()
                }
            };
            let source = skill_source(skill.origin, &skill.path, place.workdir(), place.roots());
            let dir = match skill.origin {
                SkillCatalogOrigin::Embedded => None,
                SkillCatalogOrigin::Filesystem | SkillCatalogOrigin::Virtual => {
                    (!skill.dir.as_os_str().is_empty())
                        .then(|| skill.dir.to_string_lossy().into_owned())
                }
            };
            SkillInfo {
                name: skill.name,
                description: skill.description,
                location,
                dir,
                path: skill.path.to_string_lossy().into_owned(),
                content: skill.content,
                source,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_source_tells_builtin_bundle_project_and_user_apart() {
        let workdir = Path::new("/home/u/work");
        let roots = [PathBuf::from("/srv/shared")];
        let source =
            |origin, path: &str| skill_source(origin, Path::new(path), Some(workdir), &roots);
        assert_eq!(
            source(
                SkillCatalogOrigin::Embedded,
                "embedded:hya/core-skills/skill/x/SKILL.md"
            ),
            "builtin"
        );
        assert_eq!(
            source(SkillCatalogOrigin::Virtual, "bundle://me/x/SKILL.md"),
            "bundle"
        );
        assert_eq!(
            source(
                SkillCatalogOrigin::Filesystem,
                "/home/u/work/.hya/skills/x/SKILL.md"
            ),
            "project"
        );
        assert_eq!(
            source(
                SkillCatalogOrigin::Filesystem,
                "/srv/shared/.agents/skills/x/SKILL.md"
            ),
            "project"
        );
        assert_eq!(
            source(
                SkillCatalogOrigin::Filesystem,
                "/home/u/.codex/skills/x/SKILL.md"
            ),
            "user"
        );
        // Without a directory (the global view) every file skill is the user's.
        assert_eq!(
            skill_source(
                SkillCatalogOrigin::Filesystem,
                Path::new("/home/u/work/.hya/skills/x/SKILL.md"),
                None,
                &[]
            ),
            "user"
        );
    }
}
