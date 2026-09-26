use std::path::Path;

use hya_tool::{
    SkillCatalogOrigin, discover_skills_with_builtins, discover_user_skills_with_builtins,
};
use serde::Serialize;

#[derive(Clone, Serialize)]
pub(crate) struct SkillInfo {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) location: String,
    pub(crate) content: String,
}

/// Skills visible in `workdir`; with no workdir, user skills and builtins.
pub(crate) fn list(workdir: Option<&Path>) -> Vec<SkillInfo> {
    workdir
        .map_or_else(
            discover_user_skills_with_builtins,
            discover_skills_with_builtins,
        )
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
