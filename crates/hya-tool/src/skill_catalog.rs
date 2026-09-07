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

/// Default skill search roots for a project workdir (project + user + compat paths).
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

/// Discover skills under the default roots for `workdir`.
#[must_use]
pub fn discover_skills(workdir: &Path) -> Vec<SkillCatalogEntry> {
    discover_skills_from_dirs(&skill_dirs_for_workdir(workdir))
}
/// Discover native skills, then append compiled builtins that were not
/// overridden by a native entry of the same name.
#[must_use]
pub fn discover_skills_with_builtins(workdir: &Path) -> Vec<SkillCatalogEntry> {
    merge_skill_catalog(discover_skills(workdir))
}

/// Merge the authoritative embedded builtin catalog after native entries.
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

const CUSTOMIZE_COMPAT_DESCRIPTION: &str = "Use ONLY when the user is editing or creating compat's own configuration: opencode.json, opencode.jsonc, files under .opencode/, or files under ~/.config/opencode/. Also use when creating or fixing compat skills, plugins, MCP servers, or permission rules. Do not use for native agent authoring (see agent-bundle-authoring), the user's own application code, or any project that is not configuring compat itself.";
const AGENT_BUNDLE_AUTHORING_DESCRIPTION: &str = "Use when authoring and packaging public AgentBundles (one agent per bundle): static process-free bundles or activation-scoped Bun Compat sidecars, exact bundle.hya.md closure, install/info commands, stable AgentName bytes, role/can_spawn/lifecycle, harness resource views, and private/unsupported boundaries. Do not use for compat opencode.json customization, external model loops, raw Rust activation, or Bundle-declared MCP.";
const SECURE_SELF_UPDATE_DESCRIPTION: &str = "Use when verifying, staging, recovering, or owner-activating an independent hya release with hya-updater: signed metadata, local package fetch, immutable staging, smoke subprocess, activation journal/selector, anti-rollback floor, and install.sh break-glass. Do not use for bundle install, plugin load, or to skip the owner activation gate.";

const CUSTOMIZE_COMPAT_BODY: &str = include_str!("skill_templates/customize-compat.md");
const AGENT_BUNDLE_AUTHORING_BODY: &str = include_str!("skill_templates/agent-bundle-authoring.md");
const SECURE_SELF_UPDATE_BODY: &str = include_str!("skill_templates/secure-self-update.md");

/// Return the authoritative compiled builtin Skill entries.
#[must_use]
pub fn builtin_skills() -> Vec<SkillCatalogEntry> {
    [
        (
            "customize-compat",
            CUSTOMIZE_COMPAT_DESCRIPTION,
            CUSTOMIZE_COMPAT_BODY,
        ),
        (
            "agent-bundle-authoring",
            AGENT_BUNDLE_AUTHORING_DESCRIPTION,
            AGENT_BUNDLE_AUTHORING_BODY,
        ),
        (
            "secure-self-update",
            SECURE_SELF_UPDATE_DESCRIPTION,
            SECURE_SELF_UPDATE_BODY,
        ),
    ]
    .into_iter()
    .map(|(name, description, content)| SkillCatalogEntry {
        name: name.to_string(),
        description: description.to_string(),
        content: content.to_string(),
        allowed_tools: Vec::new(),
        model: None,
        path: PathBuf::from(format!("embedded:hya/skill/{name}/SKILL.md")),
        dir: PathBuf::new(),
        origin: SkillCatalogOrigin::Embedded,
    })
    .collect()
}

/// Whether an entry has no filesystem base and is compiled into the binary.
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
