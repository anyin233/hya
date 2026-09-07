use std::path::Path;

use hya_tool::{SkillCatalogOrigin, discover_skills_with_builtins};
use serde::Serialize;

#[derive(Clone, Serialize)]
pub(in crate::compat) struct SkillInfo {
    pub(in crate::compat) name: String,
    pub(in crate::compat) description: String,
    pub(in crate::compat) location: String,
    pub(in crate::compat) content: String,
}

pub(in crate::compat) fn list(workdir: &Path) -> Vec<SkillInfo> {
    discover_skills_with_builtins(workdir)
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
