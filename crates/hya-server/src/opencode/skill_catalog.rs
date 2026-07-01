use std::path::Path;

use hya_tool::discover_skills;

use serde::Serialize;

const CUSTOMIZE_OPENCODE_BODY: &str = include_str!("skill_templates/customize-opencode.md");
const CUSTOMIZE_OPENCODE_DESCRIPTION: &str = "Use ONLY when the user is editing or creating opencode's own configuration: opencode.json, opencode.jsonc, files under .opencode/, or files under ~/.config/opencode/. Also use when creating or fixing opencode agents, subagents, skills, plugins, MCP servers, or permission rules. Do not use for the user's own application code, or for any project that is not configuring opencode itself.";

#[derive(Clone, Serialize)]
pub(in crate::opencode) struct SkillInfo {
    pub(in crate::opencode) name: String,
    pub(in crate::opencode) description: String,
    pub(in crate::opencode) location: String,
    pub(in crate::opencode) content: String,
}

pub(in crate::opencode) fn list(workdir: &Path) -> Vec<SkillInfo> {
    let mut skills = discover_skills(workdir)
        .into_iter()
        .map(|skill| SkillInfo {
            name: skill.name,
            description: skill.description,
            location: skill.path.to_string_lossy().into_owned(),
            content: skill.content,
        })
        .collect::<Vec<_>>();
    if !skills
        .iter()
        .any(|skill| skill.name == "customize-opencode")
    {
        skills.push(SkillInfo {
            name: "customize-opencode".to_string(),
            description: CUSTOMIZE_OPENCODE_DESCRIPTION.to_string(),
            location: "<built-in>".to_string(),
            content: CUSTOMIZE_OPENCODE_BODY.to_string(),
        });
    }
    skills
}
