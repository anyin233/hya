//! Workdir-scoped custom slash-command discovery from hya-native directories.
//!
//! Commands are markdown files under `{workdir}/.hya/command` and
//! `{workdir}/.hya/commands` (recursive); optional YAML frontmatter carries
//! `description`, `agent`, `model`, and `subtask`. Inline config-file command
//! tables are not a source.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::support::command_catalog::CommandInfo;

#[derive(Default, Deserialize)]
struct CommandFrontmatter {
    description: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    subtask: Option<bool>,
}

pub(super) fn disk_commands(workdir: &Path) -> Vec<CommandInfo> {
    let mut files = Vec::new();
    for root in [workdir.join(".hya/command"), workdir.join(".hya/commands")] {
        collect_markdown_files(&root, &root, &mut files);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));

    let mut commands = Vec::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file.path) else {
            continue;
        };
        let Some((frontmatter, template)) = parse_command_file(&content) else {
            continue;
        };
        commands.push(CommandInfo::command(
            file.name,
            frontmatter.description,
            frontmatter.agent,
            frontmatter.model,
            template,
            frontmatter.subtask,
        ));
    }
    commands
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
