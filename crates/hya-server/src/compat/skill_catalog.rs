use std::path::Path;

use hya_tool::discover_skills;

use serde::Serialize;

const CUSTOMIZE_COMPAT_BODY: &str = include_str!("skill_templates/customize-compat.md");
const CUSTOMIZE_COMPAT_DESCRIPTION: &str = "Use ONLY when the user is editing or creating compat's own configuration: opencode.json, opencode.jsonc, files under .opencode/, or files under ~/.config/opencode/. Also use when creating or fixing compat agents, subagents, skills, plugins, MCP servers, or permission rules. Do not use for the user's own application code, or for any project that is not configuring compat itself.";

#[derive(Clone, Serialize)]
pub(in crate::compat) struct SkillInfo {
    pub(in crate::compat) name: String,
    pub(in crate::compat) description: String,
    pub(in crate::compat) location: String,
    pub(in crate::compat) content: String,
}

pub(in crate::compat) fn list(workdir: &Path) -> Vec<SkillInfo> {
    let mut skills = discover_skills(workdir)
        .into_iter()
        .map(|skill| SkillInfo {
            name: skill.name,
            description: skill.description,
            location: skill.path.to_string_lossy().into_owned(),
            content: skill.content,
        })
        .collect::<Vec<_>>();
    if !skills.iter().any(|skill| skill.name == "customize-compat") {
        skills.push(SkillInfo {
            name: "customize-compat".to_string(),
            description: CUSTOMIZE_COMPAT_DESCRIPTION.to_string(),
            location: "<built-in>".to_string(),
            content: CUSTOMIZE_COMPAT_BODY.to_string(),
        });
    }
    skills
}
