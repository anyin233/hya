use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub struct Skill {
    pub name: String,
    pub description: String,
    pub content: String,
}

#[must_use]
pub fn parse_skill(content: &str) -> Option<Skill> {
    let (front, body) = content.strip_prefix("---")?.split_once("\n---")?;
    let mut name = None;
    let mut description = None;
    for line in front.lines() {
        if let Some(v) = line.strip_prefix("name:") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("description:") {
            description = Some(v.trim().to_string());
        }
    }
    Some(Skill {
        name: name?,
        description: description?,
        content: body.strip_prefix('\n').unwrap_or(body).to_string(),
    })
}

#[must_use]
pub fn skill_dirs_for_workdir(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        workdir.join(".opencode/skill"),
        workdir.join(".opencode/skills"),
        workdir.join(".hya/skills"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".config/hya/skills"));
    }
    dirs
}

#[must_use]
pub fn discover_skills(dirs: &[PathBuf]) -> Vec<Skill> {
    let mut skills = Vec::new();
    for d in dirs {
        let Ok(entries) = std::fs::read_dir(d) else {
            continue;
        };
        for entry in entries.flatten() {
            let skill_md = entry.path().join("SKILL.md");
            if let Ok(content) = std::fs::read_to_string(&skill_md)
                && let Some(s) = parse_skill(&content)
            {
                skills.push(s);
            }
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

#[must_use]
pub fn skills_section(skills: &[Skill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut s = String::from(
        "These skills are available on demand; read the named SKILL.md when relevant:\n",
    );
    for sk in skills {
        let _ = writeln!(s, "- {}: {}", sk.name, sk.description);
    }
    Some(s)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tempdir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hya-skill-{nanos}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_valid_frontmatter_and_content() {
        let md = "---\nname: committer\ndescription: writes commits\n---\nbody here";

        let skill = parse_skill(md).unwrap();

        assert_eq!(skill.name, "committer");
        assert_eq!(skill.description, "writes commits");
        assert_eq!(skill.content, "body here");
    }

    #[test]
    fn rejects_missing_fields_or_frontmatter() {
        assert!(parse_skill("---\nname: x\n---\n").is_none());
        assert!(parse_skill("no frontmatter").is_none());
    }

    #[test]
    fn skill_dirs_for_workdir_include_current_hya_and_opencode_project_roots() {
        let root = tempdir();
        let dirs = skill_dirs_for_workdir(&root);

        assert!(dirs.contains(&root.join(".opencode/skill")));
        assert!(dirs.contains(&root.join(".opencode/skills")));
        assert!(dirs.contains(&root.join(".hya/skills")));
        if let Some(home) = std::env::var_os("HOME") {
            assert!(dirs.contains(&PathBuf::from(home).join(".config/hya/skills")));
        }
    }

    #[test]
    fn discovers_and_formats_skills() {
        let root = tempdir();
        let skill_dir = root.join("committer");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: committer\ndescription: writes commits\n---\nbody",
        )
        .unwrap();
        let found = discover_skills(std::slice::from_ref(&root));
        assert_eq!(found.len(), 1);
        let section = skills_section(&found).unwrap();
        assert!(section.contains("committer"));
        assert!(section.contains("writes commits"));
        assert!(skills_section(&[]).is_none());
    }
}
