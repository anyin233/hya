use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

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
    /// `disable-model-invocation: true`: left out of the model's skill
    /// index; `/skill:<name>` still runs it.
    pub disable_model_invocation: bool,
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
    /// `disable-model-invocation: true`: left out of the model's skill
    /// index (its body still loads by name, and `/skill:<name>` runs it).
    pub disable_model_invocation: bool,
}

/// What is wrong with a discovered `SKILL.md` ([`SkillDiagnostic`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkillDiagnosticKind {
    /// No or unclosed frontmatter, or YAML that does not parse: skipped.
    Invalid,
    /// No `name`: skipped.
    MissingName,
    /// No `description`: skipped.
    MissingDescription,
    /// A skill of the same name was found first: skipped.
    Collision,
    /// The name breaks the Agent Skills rule (lowercase letters, digits,
    /// single hyphens, at most 64 characters): listed, with a warning.
    Name,
    /// The description is over 1024 characters: listed, with a warning.
    DescriptionLength,
}

impl SkillDiagnosticKind {
    /// Wire name (`CatalogDiagnostic.kind`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::MissingName => "missing_name",
            Self::MissingDescription => "missing_description",
            Self::Collision => "collision",
            Self::Name => "name",
            Self::DescriptionLength => "description_length",
        }
    }
}

/// A `SKILL.md` discovery skipped or warns about, as pi reports its skill
/// diagnostics. `disable: true` is deliberate and is not one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillDiagnostic {
    /// The `SKILL.md` it is about.
    pub path: PathBuf,
    /// What is wrong.
    pub kind: SkillDiagnosticKind,
    /// One line for a person, naming the file.
    pub message: String,
}

/// Skills found in a set of roots, and what was wrong with the others.
#[derive(Clone, Debug, Default)]
pub struct SkillDiscovery {
    /// The catalog, first name wins.
    pub skills: Vec<SkillCatalogEntry>,
    /// Files skipped or warned about, in discovery order.
    pub diagnostics: Vec<SkillDiagnostic>,
}

/// Why a `SKILL.md` did not parse into a skill.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SkillParseError {
    /// The file does not start with a `---` fence.
    NoFrontmatter,
    /// The closing `---` fence is missing.
    UnclosedFrontmatter,
    /// The frontmatter is not valid YAML of the expected shape.
    Yaml(String),
    /// No `name`.
    MissingName,
    /// No `description`.
    MissingDescription,
    /// `disable: true`.
    Disabled,
}

/// The longest skill name the Agent Skills rule allows.
pub const MAX_SKILL_NAME_CHARS: usize = 64;
/// The longest skill description the Agent Skills rule allows.
pub const MAX_SKILL_DESCRIPTION_CHARS: usize = 1024;

/// What breaks the Agent Skills naming rule in `name` (lowercase ASCII
/// letters, digits, and hyphens; no leading, trailing, or doubled hyphen;
/// at most 64 characters), or `None`.
#[must_use]
pub fn skill_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("is empty");
    }
    if name.chars().count() > MAX_SKILL_NAME_CHARS {
        return Some("is longer than 64 characters");
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Some("may use only lowercase letters, digits, and hyphens");
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return Some("may not start or end with a hyphen or repeat one");
    }
    None
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

/// Whether an entry comes from the trusted `hya/core-skills` bundle rather than a Skill directory.
#[must_use]
pub fn is_embedded_skill(skill: &SkillCatalogEntry) -> bool {
    skill.origin == SkillCatalogOrigin::Embedded
}

/// Format an `available_skills` system-prompt section, or `None` when empty.
#[must_use]
pub fn skills_section(skills: &[SkillCatalogEntry]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let skills = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect::<Vec<_>>();
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

/// Session-scoped access to the immutable skill catalog selected by the host.
#[derive(Clone, Default)]
pub struct SkillPlane {
    skills: Arc<Vec<SkillCatalogEntry>>,
}

impl SkillPlane {
    /// Build a plane over the exact immutable catalog captured by a turn.
    #[must_use]
    pub fn from_snapshot(skills: Arc<Vec<SkillCatalogEntry>>) -> Self {
        Self { skills }
    }

    /// Skill body for `skill://<name>`, without the `skill` tool's framing.
    ///
    /// Returns `None` for an unknown name so the router can report it as a
    /// missing handle rather than leaking a tool-level error type.
    pub(crate) fn body(&self, workdir: &Path, name: &str) -> Option<String> {
        self.require(workdir, name).ok().map(|info| info.content)
    }

    /// Resolve a named skill against this plane's captured catalog.
    ///
    /// # Errors
    /// Returns [`SkillError::NotFound`] when the name is unavailable.
    pub fn require(&self, _workdir: &Path, name: &str) -> Result<SkillInfo, SkillError> {
        let skill = self.skills.iter().find(|skill| skill.name == name).cloned();
        let Some(skill) = skill else {
            return Err(SkillError::NotFound(name.to_string()));
        };
        let dir = match skill.origin {
            SkillCatalogOrigin::Embedded => None,
            SkillCatalogOrigin::Filesystem | SkillCatalogOrigin::Virtual => {
                Some(canonical_or_self(&skill.dir))
            }
        };
        Ok(SkillInfo {
            name: skill.name,
            dir,
            origin: skill.origin,
            content: skill.content,
        })
    }
}

/// Failure to resolve a skill from the captured catalog.
#[derive(Debug, Error)]
pub enum SkillError {
    /// No skill with the requested name exists in the captured catalog.
    #[error("skill not found: {0}")]
    NotFound(String),
}

/// Resolved skill body and provenance from a session's captured catalog.
pub struct SkillInfo {
    /// Canonical skill name.
    pub name: String,
    /// Filesystem base when the skill is not embedded.
    pub dir: Option<PathBuf>,
    /// Source class used to frame the tool result.
    pub origin: SkillCatalogOrigin,
    /// Markdown body.
    pub content: String,
}

fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
