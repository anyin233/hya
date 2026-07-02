use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSkill {
    pub name: String,
    pub description: String,
    pub content: String,
    /// Per-skill tool allowlist from `allowed-tools`. Empty = no restriction.
    pub allowed_tools: Vec<String>,
    /// Optional per-skill model override.
    pub model: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillCatalogEntry {
    pub name: String,
    pub description: String,
    pub content: String,
    pub allowed_tools: Vec<String>,
    pub model: Option<String>,
    pub path: PathBuf,
    pub dir: PathBuf,
}

/// YAML frontmatter shape for a `SKILL.md`. Every field beyond name/description is
/// optional so existing minimal skills keep parsing.
#[derive(Debug, Default, Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(default, rename = "allowed-tools")]
    allowed_tools: Vec<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    disable: bool,
    #[serde(default)]
    #[allow(dead_code)]
    license: Option<String>,
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
                allowed_tools: parsed.allowed_tools,
                model: parsed.model,
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

/// Parse a `SKILL.md`: YAML frontmatter between `---` fences, then the markdown
/// body. Requires `name` and `description`; returns `None` for malformed or
/// `disable: true` skills so they are skipped during discovery.
#[must_use]
pub fn parse_skill(content: &str) -> Option<ParsedSkill> {
    let after = content.strip_prefix("---")?;
    let (front, body) = after.split_once("\n---")?;
    let front: SkillFrontmatter = serde_norway::from_str(front).ok()?;
    if front.disable {
        return None;
    }
    Some(ParsedSkill {
        name: front.name?,
        description: front.description?,
        content: body.strip_prefix('\n').unwrap_or(body).to_string(),
        allowed_tools: front.allowed_tools,
        model: front.model,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn parses_frontmatter_policy_fields() {
        let md = "---\nname: reviewer\ndescription: reviews code\nallowed-tools: [read, grep]\nmodel: anthropic/claude-sonnet-4-6\nlicense: MIT\n---\nBODY TEXT\n";
        let parsed = parse_skill(md).expect("parses");
        assert_eq!(parsed.name, "reviewer");
        assert_eq!(parsed.description, "reviews code");
        assert_eq!(parsed.allowed_tools, vec!["read", "grep"]);
        assert_eq!(parsed.model.as_deref(), Some("anthropic/claude-sonnet-4-6"));
        assert_eq!(parsed.content, "BODY TEXT\n");
    }

    #[test]
    fn minimal_frontmatter_still_parses_with_defaults() {
        let md = "---\nname: mini\ndescription: tiny\n---\nbody";
        let parsed = parse_skill(md).expect("parses");
        assert!(parsed.allowed_tools.is_empty());
        assert!(parsed.model.is_none());
    }

    #[test]
    fn disabled_skill_is_skipped() {
        let md = "---\nname: off\ndescription: nope\ndisable: true\n---\nbody";
        assert!(parse_skill(md).is_none(), "disabled skills are skipped");
    }
}
