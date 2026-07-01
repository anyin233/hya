use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSkill {
    pub name: String,
    pub description: String,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillCatalogEntry {
    pub name: String,
    pub description: String,
    pub content: String,
    pub path: PathBuf,
    pub dir: PathBuf,
}

#[must_use]
pub fn skill_dirs_for_workdir(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![workdir.join(".hya/skills")];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".config/hya/skills"));
        dirs.push(home.join(".claude/skills"));
        dirs.push(home.join(".config/opencode/skills"));
        dirs.push(home.join(".config/opencode/skill"));
    }
    dirs.push(workdir.join(".opencode/skills"));
    dirs.push(workdir.join(".opencode/skill"));
    dirs.push(workdir.join(".agents/skills"));
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".codex/skills"));
        dirs.push(home.join(".agents/skills"));
    }
    dirs
}

#[must_use]
pub fn discover_skills(workdir: &Path) -> Vec<SkillCatalogEntry> {
    discover_skills_from_dirs(&skill_dirs_for_workdir(workdir))
}

#[must_use]
pub fn discover_skills_from_dirs(dirs: &[PathBuf]) -> Vec<SkillCatalogEntry> {
    let mut seen = HashSet::new();
    let mut skills = Vec::new();

    for root in dirs {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(std::fs::DirEntry::path);

        for entry in entries {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let path = entry.path().join("SKILL.md");
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(parsed) = parse_skill(&raw) else {
                continue;
            };
            if !seen.insert(parsed.name.clone()) {
                continue;
            }
            let dir = path
                .parent()
                .map_or_else(|| entry.path(), std::path::Path::to_path_buf);
            skills.push(SkillCatalogEntry {
                name: parsed.name,
                description: parsed.description,
                content: parsed.content,
                path,
                dir,
            });
        }
    }

    skills
}

#[must_use]
pub fn skills_section(skills: &[SkillCatalogEntry]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut section =
        "These skills are available on demand; read the named SKILL.md when relevant:".to_string();
    for skill in skills {
        section.push_str("\n- ");
        section.push_str(&skill.name);
        section.push_str(": ");
        section.push_str(&skill.description);
    }
    Some(section)
}

#[must_use]
pub fn parse_skill(content: &str) -> Option<ParsedSkill> {
    let after = content.strip_prefix("---")?;
    let (front, body) = after.split_once("\n---")?;
    let mut name = None;
    let mut description = None;
    for line in front.lines() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().to_string());
        }
    }
    Some(ParsedSkill {
        name: name?,
        description: description?,
        content: body.strip_prefix('\n').unwrap_or(body).to_string(),
    })
}
