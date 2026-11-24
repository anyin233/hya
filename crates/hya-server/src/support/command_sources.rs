//! Custom slash-command discovery from hya-native directories.
//!
//! Commands are markdown files under `{dir}/.hya/command` and
//! `{dir}/.hya/commands` of the directory and each Project root (`project`),
//! then `~/.config/hya/command` and `~/.config/hya/commands` (`user`; all
//! recursive). Optional YAML frontmatter carries `description` (else the
//! first non-empty line of the body), `argument-hint`, `agent`, `model`, and
//! `subtask`. Inline config-file command tables are not a source.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::support::command_catalog::{CommandInfo, SKILL_COMMAND_PREFIX};

#[derive(Default, Deserialize)]
struct CommandFrontmatter {
    description: Option<String>,
    #[serde(rename = "argument-hint")]
    argument_hint: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    subtask: Option<bool>,
}

/// A command file the catalog skipped (`CatalogDiagnostic`).
#[derive(Clone, Debug)]
pub(crate) struct CommandDiagnostic {
    pub(crate) path: PathBuf,
    /// `reserved` (named `skill:…`) or `invalid` (frontmatter that does not parse).
    pub(crate) kind: &'static str,
    pub(crate) message: String,
}

/// The longest description taken from a command's first line.
const FIRST_LINE_DESCRIPTION_CHARS: usize = 100;

/// The user command roots, `~/.config/hya/command` and
/// `~/.config/hya/commands`; none without `HOME`.
pub(super) fn user_command_roots() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| {
            vec![
                home.join(".config/hya/command"),
                home.join(".config/hya/commands"),
            ]
        })
        .unwrap_or_default()
}

/// The commands of a directory's `.hya/command` and `.hya/commands`.
pub(super) fn disk_commands(
    workdir: &Path,
    scope: &'static str,
    diagnostics: &mut Vec<CommandDiagnostic>,
) -> Vec<CommandInfo> {
    root_commands(
        &[workdir.join(".hya/command"), workdir.join(".hya/commands")],
        scope,
        diagnostics,
    )
}

/// The commands under `roots` (each scanned recursively; names relative to
/// their root), sorted by path.
pub(super) fn root_commands(
    roots: &[PathBuf],
    scope: &'static str,
    diagnostics: &mut Vec<CommandDiagnostic>,
) -> Vec<CommandInfo> {
    let mut files = Vec::new();
    for root in roots {
        collect_markdown_files(root, root, &mut files);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));

    let mut commands = Vec::new();
    for file in files {
        // `skill:` names the skill namespace (`/skill:<name>`); a command
        // file cannot take it.
        if file.name.starts_with(SKILL_COMMAND_PREFIX) {
            diagnostics.push(CommandDiagnostic {
                message: format!(
                    "{}: `/{}` is in the reserved skill namespace; skipped",
                    file.path.display(),
                    file.name
                ),
                path: file.path,
                kind: "reserved",
            });
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&file.path) else {
            continue;
        };
        let Some((frontmatter, template)) = parse_command_file(&content) else {
            diagnostics.push(CommandDiagnostic {
                message: format!(
                    "{}: the frontmatter does not parse; skipped",
                    file.path.display()
                ),
                path: file.path,
                kind: "invalid",
            });
            continue;
        };
        let description = frontmatter
            .description
            .or_else(|| first_line_description(&template));
        commands.push(
            CommandInfo::command(
                file.name,
                description,
                frontmatter.agent,
                frontmatter.model,
                template,
                frontmatter.subtask,
            )
            .with_argument_hint(frontmatter.argument_hint)
            .with_scope(scope),
        );
    }
    commands
}

/// pi's description of a template without one: its first non-empty line,
/// cut to 100 characters.
fn first_line_description(template: &str) -> Option<String> {
    let line = template
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    if line.chars().count() <= FIRST_LINE_DESCRIPTION_CHARS {
        return Some(line.to_string());
    }
    let cut = line
        .chars()
        .take(FIRST_LINE_DESCRIPTION_CHARS - 1)
        .collect::<String>();
    Some(format!("{}…", cut.trim_end()))
}

struct CommandFile {
    name: String,
    path: PathBuf,
}

fn collect_markdown_files(base: &Path, dir: &Path, files: &mut Vec<CommandFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(base, &path, files);
        } else if path.extension().is_some_and(|extension| extension == "md")
            && let Some(name) = command_name(base, &path)
        {
            files.push(CommandFile { name, path });
        }
    }
}

fn command_name(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let name = relative
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    name.strip_suffix(".md").map(str::to_string)
}

fn parse_command_file(content: &str) -> Option<(CommandFrontmatter, String)> {
    let Some((frontmatter, body)) = split_frontmatter(content) else {
        return Some((CommandFrontmatter::default(), content.trim().to_string()));
    };
    let metadata = if frontmatter.trim().is_empty() {
        CommandFrontmatter::default()
    } else {
        serde_norway::from_str(frontmatter).ok()?
    };
    Some((metadata, body.trim().to_string()))
}

fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    let (frontmatter, body) = rest.split_once("\n---")?;
    Some((
        frontmatter.strip_suffix('\r').unwrap_or(frontmatter),
        body.strip_prefix("\r\n")
            .or_else(|| body.strip_prefix('\n'))
            .unwrap_or(body),
    ))
}
