use hya_tool::{
    SkillCatalogEntry, SkillCatalogOrigin, SkillDiagnostic,
    discover_skills_for_roots_with_diagnostics, discover_user_skills_with_diagnostics,
};
use std::path::{Path, PathBuf};

use hya_proto::SessionId;
use serde::Serialize;

use crate::ServerState;
use crate::support::catalog_place::CatalogPlace;

#[derive(Clone, Serialize)]
pub(crate) struct SkillInfo {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) location: String,
    /// The skill's own directory, the base of its relative references;
    /// `None` for a built-in or bundle skill (no filesystem base).
    #[serde(skip)]
    pub(crate) dir: Option<String>,
    /// The catalog path (`embedded:hya/core-skills/skill/<name>/SKILL.md`
    /// for a built-in skill), named by an expanded `/skill:<name>` block.
    #[serde(skip)]
    pub(crate) path: String,
    pub(crate) content: String,
    /// `builtin`, `project`, `user`, or `bundle` (`SkillSummary.source`).
    pub(crate) source: &'static str,
    /// `disable-model-invocation: true`: out of the model's skill index.
    pub(crate) disable_model_invocation: bool,
    /// The bundle (`hya/alpha`) or plugin (`plugin:toolbox`) of a `bundle`
    /// skill.
    pub(crate) bundle_id: Option<String>,
}

/// A place's skills and the diagnostics of the skill files it skipped or
/// warns about.
pub(crate) struct SkillListing {
    pub(crate) skills: Vec<SkillInfo>,
    pub(crate) diagnostics: Vec<SkillDiagnostic>,
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

/// The bundle or plugin that owns a runtime skill, from its catalog path:
/// `bundle:<publisher>/<name>/…` → `<publisher>/<name>`, `plugin:<id>/…` →
/// `plugin:<id>`.
pub(crate) fn virtual_owner(path: &str) -> Option<String> {
    if let Some(rest) = path.strip_prefix("bundle:") {
        let mut parts = rest.splitn(3, '/');
        let publisher = parts.next().filter(|part| !part.is_empty())?;
        let name = parts.next().filter(|part| !part.is_empty())?;
        return Some(format!("{publisher}/{name}"));
    }
    let rest = path.strip_prefix("plugin:")?;
    let id = rest.split('/').next().filter(|part| !part.is_empty())?;
    Some(format!("plugin:{id}"))
}

/// One catalog entry as listed at `place`.
pub(crate) fn info(skill: SkillCatalogEntry, place: &CatalogPlace) -> SkillInfo {
    let path = skill.path.to_string_lossy().into_owned();
    let location = match skill.origin {
        SkillCatalogOrigin::Embedded => "<built-in>".to_string(),
        SkillCatalogOrigin::Filesystem | SkillCatalogOrigin::Virtual => path.clone(),
    };
    let source = skill_source(skill.origin, &skill.path, place.workdir(), place.roots());
    // Only a real directory is a base for relative references.
    let dir = match skill.origin {
        SkillCatalogOrigin::Filesystem if skill.dir.is_absolute() => {
            Some(skill.dir.to_string_lossy().into_owned())
        }
        _ => None,
    };
    let bundle_id = match skill.origin {
        SkillCatalogOrigin::Virtual => virtual_owner(&path),
        _ => None,
    };
    SkillInfo {
        name: skill.name,
        description: skill.description,
        location,
        dir,
        path,
        content: skill.content,
        source,
        disable_model_invocation: skill.disable_model_invocation,
        bundle_id,
    }
}

/// Skills visible at `place`: its directory first, then each Project root
/// in order (first wins); the global view lists user skills and builtins.
/// With the diagnostics of the skill files skipped or warned about.
pub(crate) fn listing(place: &CatalogPlace) -> SkillListing {
    let discovery = place
        .workdir()
        .map_or_else(discover_user_skills_with_diagnostics, |workdir| {
            discover_skills_for_roots_with_diagnostics(workdir, place.roots())
        });
    SkillListing {
        skills: discovery
            .skills
            .into_iter()
            .map(|skill| info(skill, place))
            .collect(),
        diagnostics: discovery.diagnostics,
    }
}

/// [`listing`] without the diagnostics.
pub(crate) fn list(place: &CatalogPlace) -> Vec<SkillInfo> {
    listing(place).skills
}

/// What `session`'s agent can load (`ListSkills` with `session`, and the
/// `/skill:<name>` rows its command turns expand): the place's skills plus
/// the bundle and plugin skills its agent's resource view selects. When the
/// session's binding cannot be read the directory listing stands in, so a
/// broken bundle never hides the user's own skills.
pub(crate) async fn session_listing(
    st: &ServerState,
    session: SessionId,
    place: &CatalogPlace,
) -> SkillListing {
    let mut listing = listing(place);
    match st.engine.session_skill_catalog(session).await {
        Ok(entries) => {
            for entry in entries {
                if entry.origin == SkillCatalogOrigin::Virtual
                    && !listing.skills.iter().any(|skill| skill.name == entry.name)
                {
                    listing.skills.push(info(entry, place));
                }
            }
        }
        Err(error) => {
            tracing::warn!(%session, %error, "reading the session's skill view failed; listing its directory skills");
        }
    }
    listing
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
