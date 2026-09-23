//! Discovery and parsing of `SKILL.md` catalogs for the skill plane.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Skill fields extracted from a `SKILL.md` body + frontmatter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedSkill {
    /// Skill name used by the `skill` tool.
    pub name: String,
    /// One-line description for system-prompt listings.
    pub description: String,
    /// Markdown body after frontmatter.
    pub content: String,
    /// Per-skill tool allowlist from `allowed-tools`. Empty = no restriction.
    pub allowed_tools: Vec<String>,
    /// Optional per-skill model override.
    pub model: Option<String>,
}

/// Catalog entry with metadata used by the Skill tool and prompt projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillCatalogEntry {
    /// Skill name.
    pub name: String,
    /// Short description.
    pub description: String,
    /// Full skill markdown body.
    pub content: String,
    /// Optional tool allowlist.
    pub allowed_tools: Vec<String>,
    /// Optional model override.
    pub model: Option<String>,
    /// Path-like stable identity for this catalog entry.
    pub path: PathBuf,
    /// Filesystem base directory for native/virtual entries.
    pub dir: PathBuf,
    /// How the entry was materialized.
    pub origin: SkillCatalogOrigin,
}

/// Provenance of a [`SkillCatalogEntry`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillCatalogOrigin {
    /// A `SKILL.md` discovered from a native filesystem root.
    Filesystem,
    /// A skill body compiled into the hya binary with no filesystem base.
    Embedded,
    /// A runtime-contributed resource with a virtual path.
    Virtual,
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

/// Default skill search roots for a project workdir (project + user paths).
#[must_use]
pub fn skill_dirs_for_workdir(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![workdir.join(".hya/skills")];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".config/hya/skills"));
        dirs.push(home.join(".claude/skills"));
    }
    dirs.push(workdir.join(".agents/skills"));
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".codex/skills"));
        dirs.push(home.join(".agents/skills"));
    }
    dirs
}

/// Discover skills under the default roots for `workdir`.
#[must_use]
pub fn discover_skills(workdir: &Path) -> Vec<SkillCatalogEntry> {
    discover_skills_from_dirs(&skill_dirs_for_workdir(workdir))
}
/// Discover native skills, then append bundled builtins that were not
/// overridden by a native entry of the same name.
#[must_use]
pub fn discover_skills_with_builtins(workdir: &Path) -> Vec<SkillCatalogEntry> {
    merge_skill_catalog(discover_skills(workdir))
}

/// Merge the builtin `hya/core-skills` catalog after native entries.
///
/// Native discovery owns precedence: a project/user skill with a builtin's
/// name remains the effective entry and the builtin is not duplicated.
#[must_use]
pub fn merge_skill_catalog(mut native: Vec<SkillCatalogEntry>) -> Vec<SkillCatalogEntry> {
    let mut names = native
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<HashSet<_>>();
    for builtin in builtin_skills() {
        if names.insert(builtin.name.clone()) {
            native.push(builtin);
        }
    }
    native
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreSkillFrontmatter {
    name: String,
    description: String,
}

/// One builtin Skill row: name, description, and body.
type CoreSkillRow = (String, String, String);

fn core_skills_catalog() -> &'static hya_bundle::PreparedCatalog {
    hya_bundle::first_party_bundle("hya/core-skills")
        .unwrap_or_else(|error| panic!("load builtin Skills: {error}"))
}

fn core_skill_rows() -> &'static [CoreSkillRow] {
    static ROWS: std::sync::OnceLock<Vec<CoreSkillRow>> = std::sync::OnceLock::new();
    ROWS.get_or_init(|| {
        let [bundle] = core_skills_catalog().bundles() else {
            panic!("hya/core-skills must prepare one bundle")
        };
        bundle
            .skills()
            .iter()
            .map(|resource| {
                parse_core_skill(&resource.local_id, &resource.content).unwrap_or_else(|error| {
                    panic!("hya/core-skills Skill `{}`: {error}", resource.local_id)
                })
            })
            .collect()
    })
}

fn parse_core_skill(local_id: &str, content: &str) -> Result<CoreSkillRow, String> {
    let (frontmatter, body) = content
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .ok_or("missing frontmatter")?;
    let parsed: CoreSkillFrontmatter =
        serde_norway::from_str(frontmatter).map_err(|error| error.to_string())?;
    if parsed.name != local_id || parsed.description.trim().is_empty() {
        return Err("name must match the resource id and description must be set".to_string());
    }
    Ok((parsed.name, parsed.description, body.to_string()))
}

/// Exact prepared bytes of the runtime-loaded trusted `hya/core-skills` Plugin.
///
/// # Panics
///
/// Panics when the trusted bundle is missing or invalid.
#[must_use]
pub fn core_skills_preset_bytes() -> &'static [u8] {
    core_skills_catalog().bytes()
}

/// Return the authoritative builtin Skill entries from `hya/core-skills`.
///
/// # Panics
///
/// Panics when the trusted bundle is missing or one of its Skills is invalid.
#[must_use]
pub fn builtin_skills() -> Vec<SkillCatalogEntry> {
    core_skill_rows()
        .iter()
        .map(|(name, description, content)| SkillCatalogEntry {
            name: name.clone(),
            description: description.clone(),
            content: content.clone(),
            allowed_tools: Vec::new(),
            model: None,
            path: PathBuf::from(format!("embedded:hya/core-skills/skill/{name}/SKILL.md")),
            dir: PathBuf::new(),
            origin: SkillCatalogOrigin::Embedded,
        })
        .collect()
}

/// Whether an entry comes from the trusted `hya/core-skills` bundle rather than a Skill directory.
#[must_use]
pub fn is_embedded_skill(skill: &SkillCatalogEntry) -> bool {
    skill.origin == SkillCatalogOrigin::Embedded
}

/// Discover unique skills from explicit directory roots (first name wins).
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
                origin: SkillCatalogOrigin::Filesystem,
            });
        }
    }

    skills
}

/// Format an `available_skills` system-prompt section, or `None` when empty.
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
