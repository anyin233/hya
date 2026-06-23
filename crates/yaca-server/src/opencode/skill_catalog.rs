use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    let mut skills = BTreeMap::from([(
        "customize-opencode".to_string(),
        SkillInfo {
            name: "customize-opencode".to_string(),
            description: CUSTOMIZE_OPENCODE_DESCRIPTION.to_string(),
            location: "<built-in>".to_string(),
            content: CUSTOMIZE_OPENCODE_BODY.to_string(),
        },
    )]);
    for skill in discover_disk_skills(workdir) {
        skills.insert(skill.name.clone(), skill);
    }
    skills.into_values().collect()
}

fn discover_disk_skills(workdir: &Path) -> Vec<SkillInfo> {
    let mut skills = Vec::new();
    for dir in skill_dirs(workdir) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path().join("SKILL.md");
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some((name, description, body)) = parse_skill(&content) {
                skills.push(SkillInfo {
                    name,
                    description,
                    location: path.to_string_lossy().into_owned(),
                    content: body,
                });
            }
        }
    }
    skills
}

fn parse_skill(content: &str) -> Option<(String, String, String)> {
    let (frontmatter, body) = content.strip_prefix("---")?.split_once("\n---")?;
    let mut name = None;
    let mut description = None;
    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().to_string());
        }
    }
    Some((
        name?,
        description?,
        body.strip_prefix('\n').unwrap_or(body).to_string(),
    ))
}

fn skill_dirs(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        workdir.join(".opencode/skill"),
        workdir.join(".opencode/skills"),
        workdir.join(".yaca/skills"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".config/yaca/skills"));
    }
    dirs
}
