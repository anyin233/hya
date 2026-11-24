//! Discovery and parsing of `SKILL.md` catalogs for the skill plane.

use std::collections::{HashMap, HashSet};
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

/// Why [`parse_skill_checked`] did not produce a skill.
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
    #[serde(default, rename = "disable-model-invocation")]
    disable_model_invocation: bool,
}

/// Default skill search roots for a project workdir (project + user paths).
#[must_use]
pub fn skill_dirs_for_workdir(workdir: &Path) -> Vec<PathBuf> {
    skill_dirs(Some(workdir))
}

/// User (non-project) skill search roots, for listings with no workdir.
#[must_use]
pub fn user_skill_dirs() -> Vec<PathBuf> {
    skill_dirs(None)
}

fn skill_dirs(workdir: Option<&Path>) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs = Vec::new();
    dirs.extend(workdir.map(|workdir| workdir.join(".hya/skills")));
    if let Some(home) = &home {
        dirs.push(home.join(".config/hya/skills"));
        dirs.push(home.join(".claude/skills"));
    }
    dirs.extend(workdir.map(|workdir| workdir.join(".agents/skills")));
    if let Some(home) = &home {
        dirs.push(home.join(".codex/skills"));
        dirs.push(home.join(".agents/skills"));
    }
    dirs
}

/// Deduplicate a directory list, keeping the first occurrence of each path.
fn dedupe_dirs(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    dirs.into_iter()
        .filter(|dir| seen.insert(dir.clone()))
        .collect()
}

/// Skill search roots for a `workdir` plus a Project's additional roots.
///
/// Order: `workdir/.hya/skills` first, then each `root/.hya/skills` (in
/// root order), then the user (non-project) directories, then
/// `workdir/.agents/skills` and each `root/.agents/skills` (in root order),
/// then the `.codex`/`.agents` user directories as today. Directories are
/// deduped (e.g. when a root equals `workdir`).
#[must_use]
pub fn skill_dirs_for(workdir: &Path, roots: &[PathBuf]) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs = Vec::new();

    dirs.push(workdir.join(".hya/skills"));
    for root in roots {
        dirs.push(root.join(".hya/skills"));
    }
    if let Some(home) = &home {
        dirs.push(home.join(".config/hya/skills"));
        dirs.push(home.join(".claude/skills"));
    }
    dirs.push(workdir.join(".agents/skills"));
    for root in roots {
        dirs.push(root.join(".agents/skills"));
    }
    if let Some(home) = &home {
        dirs.push(home.join(".codex/skills"));
        dirs.push(home.join(".agents/skills"));
    }

    dedupe_dirs(dirs)
}

/// Discover skills under the default roots for `workdir`.
#[must_use]
pub fn discover_skills(workdir: &Path) -> Vec<SkillCatalogEntry> {
    discover_skills_from_dirs(&skill_dirs_for_workdir(workdir))
}
/// [`discover_skills_for_roots_with_builtins`] with the diagnostics of the
/// files it skipped or warns about.
#[must_use]
pub fn discover_skills_for_roots_with_diagnostics(
    workdir: &Path,
    roots: &[PathBuf],
) -> SkillDiscovery {
    let mut discovery = discover_skills_from_dirs_with_diagnostics(&skill_dirs_for(workdir, roots));
    discovery.skills = merge_skill_catalog(discovery.skills);
    discovery
}

/// [`discover_user_skills_with_builtins`] with the diagnostics of the files
/// it skipped or warns about.
#[must_use]
pub fn discover_user_skills_with_diagnostics() -> SkillDiscovery {
    let mut discovery = discover_skills_from_dirs_with_diagnostics(&user_skill_dirs());
    discovery.skills = merge_skill_catalog(discovery.skills);
    discovery
}

/// Discover native skills, then append bundled builtins that were not
/// overridden by a native entry of the same name.
#[must_use]
pub fn discover_skills_with_builtins(workdir: &Path) -> Vec<SkillCatalogEntry> {
    merge_skill_catalog(discover_skills(workdir))
}

/// [`discover_skills_with_builtins`] without a project: user skills plus
/// bundled builtins.
#[must_use]
pub fn discover_user_skills_with_builtins() -> Vec<SkillCatalogEntry> {
    merge_skill_catalog(discover_skills_from_dirs(&user_skill_dirs()))
}

/// [`discover_skills_with_builtins`] across a Project's roots: native
/// discovery over [`skill_dirs_for`] (workdir + roots, first-found-wins by
/// name), then bundled builtins appended for names not already present.
#[must_use]
pub fn discover_skills_for_roots_with_builtins(
    workdir: &Path,
    roots: &[PathBuf],
) -> Vec<SkillCatalogEntry> {
    merge_skill_catalog(discover_skills_from_dirs(&skill_dirs_for(workdir, roots)))
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
            disable_model_invocation: false,
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
    discover_skills_from_dirs_with_diagnostics(dirs).skills
}

/// [`discover_skills_from_dirs`] with the diagnostics of the files it
/// skipped (invalid frontmatter, no name or description, a name found
/// first elsewhere) or warns about (a name or description that breaks the
/// Agent Skills rules). A directory without a readable `SKILL.md` is not a
/// skill and has no diagnostic.
#[must_use]
pub fn discover_skills_from_dirs_with_diagnostics(dirs: &[PathBuf]) -> SkillDiscovery {
    let mut first: HashMap<String, PathBuf> = HashMap::new();
    let mut discovery = SkillDiscovery::default();

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
            let mut diagnose = |kind: SkillDiagnosticKind, message: String| {
                discovery.diagnostics.push(SkillDiagnostic {
                    path: path.clone(),
                    kind,
                    message: format!("{}: {message}", path.display()),
                });
            };
            let parsed = match parse_skill_checked(&raw) {
                Ok(parsed) => parsed,
                Err(SkillParseError::Disabled) => continue,
                Err(SkillParseError::MissingName) => {
                    diagnose(
                        SkillDiagnosticKind::MissingName,
                        "no `name`; skipped".to_owned(),
                    );
                    continue;
                }
                Err(SkillParseError::MissingDescription) => {
                    diagnose(
                        SkillDiagnosticKind::MissingDescription,
                        "no `description`; skipped".to_owned(),
                    );
                    continue;
                }
                Err(error) => {
                    let reason = match error {
                        SkillParseError::NoFrontmatter => {
                            "does not start with a `---` frontmatter fence".to_owned()
                        }
                        SkillParseError::UnclosedFrontmatter => {
                            "the frontmatter has no closing `---`".to_owned()
                        }
                        SkillParseError::Yaml(error) => format!("frontmatter: {error}"),
                        _ => "invalid".to_owned(),
                    };
                    diagnose(SkillDiagnosticKind::Invalid, format!("{reason}; skipped"));
                    continue;
                }
            };
            if let Some(earlier) = first.get(&parsed.name) {
                diagnose(
                    SkillDiagnosticKind::Collision,
                    format!(
                        "`{}` is already defined by {}; skipped",
                        parsed.name,
                        earlier.display()
                    ),
                );
                continue;
            }
            if let Some(problem) = skill_name_problem(&parsed.name) {
                diagnose(
                    SkillDiagnosticKind::Name,
                    format!("the name `{}` {problem}", parsed.name),
                );
            }
            if parsed.description.chars().count() > MAX_SKILL_DESCRIPTION_CHARS {
                diagnose(
                    SkillDiagnosticKind::DescriptionLength,
                    format!(
                        "the description is longer than {MAX_SKILL_DESCRIPTION_CHARS} characters"
                    ),
                );
            }
            first.insert(parsed.name.clone(), path.clone());
            let dir = path
                .parent()
                .map_or_else(|| entry.path(), std::path::Path::to_path_buf);
            discovery.skills.push(SkillCatalogEntry {
                name: parsed.name,
                description: parsed.description,
                content: parsed.content,
                allowed_tools: parsed.allowed_tools,
                model: parsed.model,
                path,
                dir,
                origin: SkillCatalogOrigin::Filesystem,
                disable_model_invocation: parsed.disable_model_invocation,
            });
        }
    }

    discovery
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

/// Parse a `SKILL.md`: YAML frontmatter between `---` fences, then the markdown
/// body. Requires `name` and `description`; returns `None` for malformed or
/// `disable: true` skills so they are skipped during discovery.
#[must_use]
pub fn parse_skill(content: &str) -> Option<ParsedSkill> {
    parse_skill_checked(content).ok()
}

/// [`parse_skill`], saying why a file is not a skill.
///
/// # Errors
/// [`SkillParseError`]: no or unclosed frontmatter, YAML that does not
/// parse, no `name` or `description`, or `disable: true`.
pub fn parse_skill_checked(content: &str) -> Result<ParsedSkill, SkillParseError> {
    let after = content
        .strip_prefix("---")
        .ok_or(SkillParseError::NoFrontmatter)?;
    let (front, body) = after
        .split_once("\n---")
        .ok_or(SkillParseError::UnclosedFrontmatter)?;
    let front: SkillFrontmatter =
        serde_norway::from_str(front).map_err(|error| SkillParseError::Yaml(error.to_string()))?;
    if front.disable {
        return Err(SkillParseError::Disabled);
    }
    Ok(ParsedSkill {
        name: front.name.ok_or(SkillParseError::MissingName)?,
        description: front
            .description
            .ok_or(SkillParseError::MissingDescription)?,
        content: body.strip_prefix('\n').unwrap_or(body).to_string(),
        allowed_tools: front.allowed_tools,
        model: front.model,
        disable_model_invocation: front.disable_model_invocation,
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

    fn tempdir(label: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hya-skill-catalog-{label}-{}-{id}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir tempdir");
        dir
    }

    fn write_skill(dir: &Path, name: &str, description: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\nbody"),
        )
        .expect("write SKILL.md");
    }

    #[test]
    fn skill_dirs_for_orders_workdir_roots_user_then_agents() {
        let workdir = Path::new("/work");
        let roots = vec![PathBuf::from("/root-a"), PathBuf::from("/root-b")];
        let dirs = skill_dirs_for(workdir, &roots);

        let hya_positions: Vec<&PathBuf> =
            dirs.iter().filter(|d| d.ends_with(".hya/skills")).collect();
        assert_eq!(
            hya_positions,
            vec![
                &PathBuf::from("/work/.hya/skills"),
                &PathBuf::from("/root-a/.hya/skills"),
                &PathBuf::from("/root-b/.hya/skills"),
            ],
            "workdir .hya/skills first, then roots in order"
        );

        let idx = |needle: &PathBuf| dirs.iter().position(|d| d == needle).unwrap();
        let workdir_hya = idx(&PathBuf::from("/work/.hya/skills"));
        let root_a_hya = idx(&PathBuf::from("/root-a/.hya/skills"));
        let root_b_hya = idx(&PathBuf::from("/root-b/.hya/skills"));
        assert!(workdir_hya < root_a_hya);
        assert!(root_a_hya < root_b_hya);

        // User dirs come after all .hya/skills roots but before .agents/skills.
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let user_dir = home.join(".config/hya/skills");
            let user_idx = idx(&user_dir);
            assert!(user_idx > root_b_hya, "user dirs come after project roots");

            let workdir_agents_idx = idx(&PathBuf::from("/work/.agents/skills"));
            assert!(
                user_idx < workdir_agents_idx,
                "user dirs come before .agents/skills"
            );

            let root_a_agents_idx = idx(&PathBuf::from("/root-a/.agents/skills"));
            let root_b_agents_idx = idx(&PathBuf::from("/root-b/.agents/skills"));
            assert!(workdir_agents_idx < root_a_agents_idx);
            assert!(root_a_agents_idx < root_b_agents_idx);
        }
    }

    #[test]
    fn skill_dirs_for_dedupes_directories() {
        let workdir = Path::new("/work");
        // root equal to workdir must not duplicate workdir's directories.
        let roots = vec![PathBuf::from("/work"), PathBuf::from("/root-a")];
        let dirs = skill_dirs_for(workdir, &roots);

        let mut seen = HashSet::new();
        for dir in &dirs {
            assert!(seen.insert(dir.clone()), "duplicate directory: {dir:?}");
        }
    }

    #[test]
    fn skill_dirs_for_puts_workdir_before_roots() {
        let workdir = Path::new("/work");
        let roots = vec![PathBuf::from("/root-a")];
        let dirs = skill_dirs_for(workdir, &roots);

        let workdir_pos = dirs
            .iter()
            .position(|d| d == &PathBuf::from("/work/.hya/skills"))
            .expect("workdir .hya/skills present");
        let root_pos = dirs
            .iter()
            .position(|d| d == &PathBuf::from("/root-a/.hya/skills"))
            .expect("root .hya/skills present");
        assert!(workdir_pos < root_pos);
    }

    #[test]
    fn discover_skills_for_roots_first_found_wins_on_name_clash() {
        let tmp = tempdir("clash");
        let workdir = tmp.join("workdir");
        let root = tmp.join("root");
        std::fs::create_dir_all(workdir.join(".hya/skills")).expect("mkdir");
        std::fs::create_dir_all(root.join(".hya/skills")).expect("mkdir");

        write_skill(&workdir.join(".hya/skills"), "shared", "from workdir");
        write_skill(&root.join(".hya/skills"), "shared", "from root");
        write_skill(&root.join(".hya/skills"), "only-in-root", "root only");

        let dirs = skill_dirs_for(&workdir, std::slice::from_ref(&root));
        let skills = discover_skills_from_dirs(&dirs);

        let shared = skills
            .iter()
            .find(|s| s.name == "shared")
            .expect("shared skill present");
        assert_eq!(
            shared.description, "from workdir",
            "workdir entry wins over root entry with the same name"
        );
        assert!(
            skills.iter().any(|s| s.name == "only-in-root"),
            "root-only skill is still discovered"
        );
    }

    #[test]
    fn discovery_reports_skipped_and_suspicious_skill_files() {
        let tmp = tempdir("diagnostics");
        let first = tmp.join("first");
        let second = tmp.join("second");
        write_skill(&first, "shared", "from first");
        write_skill(&second, "shared", "from second");
        write_skill(&second, "Bad_Name", "listed with a warning");
        let long = "x".repeat(MAX_SKILL_DESCRIPTION_CHARS + 1);
        write_skill(&second, "long-description", &long);
        for (dir, text) in [
            ("no-fence", "name: x\n"),
            ("unclosed", "---\nname: x\ndescription: y\n"),
            ("bad-yaml", "---\nname: [x\n---\nbody"),
            ("no-name", "---\ndescription: y\n---\nbody"),
            ("no-description", "---\nname: z\n---\nbody"),
            (
                "off",
                "---\nname: off\ndescription: y\ndisable: true\n---\nbody",
            ),
        ] {
            std::fs::create_dir_all(second.join(dir)).expect("mkdir");
            std::fs::write(second.join(dir).join("SKILL.md"), text).expect("write");
        }
        let discovery =
            discover_skills_from_dirs_with_diagnostics(&[first.clone(), second.clone()]);
        let names = discovery
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["shared", "Bad_Name", "long-description"]);
        let kinds = discovery
            .diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic
                        .path
                        .parent()
                        .and_then(Path::file_name)
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    diagnostic.kind.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                ("Bad_Name".to_owned(), "name"),
                ("bad-yaml".to_owned(), "invalid"),
                ("long-description".to_owned(), "description_length"),
                ("no-description".to_owned(), "missing_description"),
                ("no-fence".to_owned(), "invalid"),
                ("no-name".to_owned(), "missing_name"),
                ("shared".to_owned(), "collision"),
                ("unclosed".to_owned(), "invalid"),
            ]
        );
        let collision = discovery
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.kind == SkillDiagnosticKind::Collision)
            .expect("collision");
        assert!(
            collision
                .message
                .contains(&first.join("shared/SKILL.md").display().to_string()),
            "{}",
            collision.message
        );
        // The plain discovery keeps the same catalog.
        assert_eq!(
            discover_skills_from_dirs(&[first, second]).len(),
            discovery.skills.len()
        );
    }

    #[test]
    fn disable_model_invocation_keeps_the_skill_out_of_the_index_only() {
        let md = "---\nname: manual\ndescription: run by hand\ndisable-model-invocation: true\n---\nbody";
        let parsed = parse_skill(md).expect("parses");
        assert!(parsed.disable_model_invocation);
        let mut hidden = builtin_skills().remove(0);
        hidden.name = "manual".to_owned();
        hidden.disable_model_invocation = true;
        let shown = builtin_skills().remove(0);
        let section = skills_section(&[hidden.clone(), shown.clone()]).expect("section");
        assert!(section.contains(&shown.name));
        assert!(!section.contains("manual"));
        assert_eq!(skills_section(&[hidden]), None);
    }

    #[test]
    fn the_naming_rule_follows_agent_skills() {
        assert_eq!(skill_name_problem("pdf-tools-2"), None);
        assert!(skill_name_problem("PDF").is_some());
        assert!(skill_name_problem("pdf_tools").is_some());
        assert!(skill_name_problem("-pdf").is_some());
        assert!(skill_name_problem("pdf--tools").is_some());
        assert!(skill_name_problem(&"a".repeat(MAX_SKILL_NAME_CHARS + 1)).is_some());
        assert!(skill_name_problem(&"a".repeat(MAX_SKILL_NAME_CHARS)).is_none());
    }
}
