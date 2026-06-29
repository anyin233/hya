use std::fmt::Write as _;
use std::path::PathBuf;

pub struct Skill {
    pub name: String,
    pub description: String,
}

#[must_use]
pub fn parse_skill(content: &str) -> Option<Skill> {
    let after = content.strip_prefix("---")?;
    let end = after.find("\n---")?;
    let front = &after[..end];
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
    })
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
    fn parses_valid_frontmatter() {
        let md = "---\nname: committer\ndescription: writes commits\n---\nbody here";
        let s = parse_skill(md).unwrap();
        assert_eq!(s.name, "committer");
        assert_eq!(s.description, "writes commits");
    }

    #[test]
    fn rejects_missing_fields_or_frontmatter() {
        assert!(parse_skill("---\nname: x\n---\n").is_none());
        assert!(parse_skill("no frontmatter").is_none());
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
