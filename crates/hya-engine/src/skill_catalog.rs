//! Discovery and parsing of `SKILL.md` catalogs for the skill plane.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub use hya_tool::{
    MAX_SKILL_DESCRIPTION_CHARS, MAX_SKILL_NAME_CHARS, ParsedSkill, SkillCatalogEntry,
    SkillCatalogOrigin, SkillDiagnostic, SkillDiagnosticKind, SkillDiscovery, SkillParseError,
    is_embedded_skill, skill_name_problem, skills_section,
};

/// Discover skills and diagnostics for a workdir and its Project roots.
/// With no workdir, return the user and trusted builtin catalog.
#[must_use]
pub fn discover(workdir: Option<&Path>, roots: &[PathBuf]) -> SkillDiscovery {
    workdir.map_or_else(discover_user_skills_with_diagnostics, |workdir| {
        discover_skills_for_roots_with_diagnostics(workdir, roots)
    })
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

    let project_dirs = crate::catalog_scope::inert_catalog_dirs(Some(workdir), roots);
    dirs.extend(project_dirs.iter().map(|dir| dir.join(".hya/skills")));
    if let Some(home) = &home {
        dirs.push(home.join(".config/hya/skills"));
        dirs.push(home.join(".claude/skills"));
    }
    dirs.extend(project_dirs.iter().map(|dir| dir.join(".agents/skills")));
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
    fn frontmatter_parsing_naming_and_model_invocation_visibility() {
        let full = parse_skill("---\nname: reviewer\ndescription: reviews code\nallowed-tools: [read, grep]\nmodel: anthropic/claude-sonnet-4-6\nlicense: MIT\n---\nBODY TEXT\n").unwrap();
        assert_eq!(
            (
                full.name.as_str(),
                full.description.as_str(),
                full.content.as_str()
            ),
            ("reviewer", "reviews code", "BODY TEXT\n")
        );
        assert_eq!(full.allowed_tools, vec!["read", "grep"]);
        assert_eq!(full.model.as_deref(), Some("anthropic/claude-sonnet-4-6"));

        let mini = parse_skill("---\nname: mini\ndescription: tiny\n---\nbody").unwrap();
        assert!(mini.allowed_tools.is_empty() && mini.model.is_none());
        assert!(
            parse_skill("---\nname: off\ndescription: nope\ndisable: true\n---\nbody").is_none()
        );

        let manual = parse_skill("---\nname: manual\ndescription: run by hand\ndisable-model-invocation: true\n---\nbody").unwrap();
        assert!(manual.disable_model_invocation);
        let mut hidden = builtin_skills().remove(0);
        hidden.name = "manual".to_owned();
        hidden.disable_model_invocation = true;
        let shown = builtin_skills().remove(0);
        let section = skills_section(&[hidden.clone(), shown.clone()]).unwrap();
        assert!(section.contains(&shown.name) && !section.contains("manual"));
        assert_eq!(skills_section(&[hidden]), None);

        assert_eq!(skill_name_problem("pdf-tools-2"), None);
        assert_eq!(skill_name_problem(&"a".repeat(MAX_SKILL_NAME_CHARS)), None);
        for bad in [
            "PDF",
            "pdf_tools",
            "-pdf",
            "pdf--tools",
            &"a".repeat(MAX_SKILL_NAME_CHARS + 1),
        ] {
            assert!(skill_name_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn skill_dirs_for_orders_and_dedupes_roots() {
        let workdir = Path::new("/work");
        let dirs = skill_dirs_for(
            workdir,
            &[
                PathBuf::from("/work"),
                PathBuf::from("/root-a"),
                PathBuf::from("/root-b"),
            ],
        );
        let mut seen = HashSet::new();
        for d in &dirs {
            assert!(seen.insert(d.clone()), "duplicate directory: {d:?}");
        }
        let hya_positions: Vec<_> = dirs.iter().filter(|d| d.ends_with(".hya/skills")).collect();
        assert_eq!(
            hya_positions,
            vec![
                &PathBuf::from("/work/.hya/skills"),
                &PathBuf::from("/root-a/.hya/skills"),
                &PathBuf::from("/root-b/.hya/skills"),
            ]
        );
        let idx = |needle: &PathBuf| dirs.iter().position(|d| d == needle).unwrap();
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            let user_idx = idx(&home.join(".config/hya/skills"));
            assert!(user_idx > idx(&PathBuf::from("/root-b/.hya/skills")));
            assert!(user_idx < idx(&PathBuf::from("/work/.agents/skills")));
        }
    }

    #[test]
    fn discovery_precedence_and_diagnostics() {
        let tmp = tempdir("diagnostics");
        let workdir = tmp.join("workdir");
        let root = tmp.join("root");
        std::fs::create_dir_all(workdir.join(".hya/skills")).unwrap();
        std::fs::create_dir_all(root.join(".hya/skills")).unwrap();
        write_skill(&workdir.join(".hya/skills"), "shared", "from workdir");
        write_skill(&root.join(".hya/skills"), "shared", "from root");
        write_skill(&root.join(".hya/skills"), "only-in-root", "root only");

        let skills =
            discover_skills_from_dirs(&skill_dirs_for(&workdir, std::slice::from_ref(&root)));
        assert_eq!(
            skills
                .iter()
                .find(|s| s.name == "shared")
                .unwrap()
                .description,
            "from workdir"
        );
        assert!(skills.iter().any(|s| s.name == "only-in-root"));

        let first = tmp.join("first");
        let second = tmp.join("second");
        write_skill(&first, "shared", "from first");
        write_skill(&second, "shared", "from second");
        write_skill(&second, "Bad_Name", "listed with a warning");
        write_skill(
            &second,
            "long-description",
            &"x".repeat(MAX_SKILL_DESCRIPTION_CHARS + 1),
        );
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
            std::fs::create_dir_all(second.join(dir)).unwrap();
            std::fs::write(second.join(dir).join("SKILL.md"), text).unwrap();
        }
        let discovery =
            discover_skills_from_dirs_with_diagnostics(&[first.clone(), second.clone()]);
        assert_eq!(
            discovery
                .skills
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["shared", "Bad_Name", "long-description"]
        );
        let kinds: Vec<_> = discovery
            .diagnostics
            .iter()
            .map(|d| {
                (
                    d.path
                        .parent()
                        .and_then(Path::file_name)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    d.kind.as_str(),
                )
            })
            .collect();
        assert_eq!(
            kinds,
            [
                ("Bad_Name".into(), "name"),
                ("bad-yaml".into(), "invalid"),
                ("long-description".into(), "description_length"),
                ("no-description".into(), "missing_description"),
                ("no-fence".into(), "invalid"),
                ("no-name".into(), "missing_name"),
                ("shared".into(), "collision"),
                ("unclosed".into(), "invalid"),
            ]
        );
        assert_eq!(
            discover_skills_from_dirs(&[first, second]).len(),
            discovery.skills.len()
        );
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
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(dir: &Path, name: &str, description: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\nbody"),
        )
        .unwrap();
    }
}
