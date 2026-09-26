use hya_tool::{
    SkillCatalogOrigin, discover_skills_for_roots_with_builtins, discover_user_skills_with_builtins,
};
use serde::Serialize;

use crate::support::catalog_place::CatalogPlace;

#[derive(Clone, Serialize)]
pub(crate) struct SkillInfo {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) location: String,
    pub(crate) content: String,
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
            SkillInfo {
                name: skill.name,
                description: skill.description,
                location,
                content: skill.content,
            }
        })
        .collect()
}
