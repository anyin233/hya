use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const INIT_TEMPLATE: &str = include_str!("command_templates/initialize.txt");
const REVIEW_TEMPLATE: &str = include_str!("command_templates/review.txt");

#[derive(Serialize)]
pub(in crate::opencode) struct CommandInfo {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    source: &'static str,
    template: String,
    hints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtask: Option<bool>,
}

#[derive(Default, Deserialize)]
struct CommandFrontmatter {
    description: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    subtask: Option<bool>,
}

pub(in crate::opencode) fn list(workdir: &Path) -> Vec<CommandInfo> {
    let workdir = workdir.to_string_lossy();
    let mut commands = vec![
        command_info(
            "init",
            "guided AGENTS.md setup",
            INIT_TEMPLATE.replace("${path}", workdir.as_ref()),
            vec!["$ARGUMENTS"],
            None,
        ),
        command_info(
            "review",
            "review changes [commit|branch|pr], defaults to uncommitted",
            REVIEW_TEMPLATE.replace("${path}", workdir.as_ref()),
            vec!["$ARGUMENTS"],
            Some(true),
        ),
        command_info(
            "help",
            "show this help",
            "/help".to_string(),
            Vec::new(),
            None,
        ),
        command_info(
            "model",
            "switch the active model",
            "/model $ARGUMENTS".to_string(),
            vec!["$ARGUMENTS"],
            None,
        ),
        command_info(
            "clear",
            "start a fresh session",
            "/clear".to_string(),
            Vec::new(),
            None,
        ),
        command_info(
            "sessions",
            "switch to another session",
            "/sessions".to_string(),
            Vec::new(),
            None,
        ),
        command_info(
            "yolo",
            "toggle auto-approval",
            "/yolo $ARGUMENTS".to_string(),
            vec!["$ARGUMENTS"],
            None,
        ),
        command_info(
            "think",
            "set reasoning effort",
            "/think $ARGUMENTS".to_string(),
            vec!["$ARGUMENTS"],
            None,
        ),
    ];
    add_disk_commands(&mut commands, Path::new(workdir.as_ref()));
    add_skill_commands(&mut commands, Path::new(workdir.as_ref()));
    commands
}

fn command_info(
    name: impl Into<String>,
    description: impl Into<String>,
    template: String,
    hints: Vec<&'static str>,
    subtask: Option<bool>,
) -> CommandInfo {
    CommandInfo {
        name: name.into(),
        description: Some(description.into()),
        agent: None,
        model: None,
        source: "command",
        template,
        hints: hints.into_iter().map(str::to_string).collect(),
        subtask,
    }
}

fn add_disk_commands(commands: &mut Vec<CommandInfo>, workdir: &Path) {
    for command in discover_disk_commands(workdir) {
        if let Some(existing) = commands.iter_mut().find(|item| item.name == command.name) {
            *existing = command;
        } else {
            commands.push(command);
        }
    }
}

fn discover_disk_commands(workdir: &Path) -> Vec<CommandInfo> {
    let mut files = Vec::new();
    for root in [
        workdir.join(".opencode/command"),
        workdir.join(".opencode/commands"),
    ] {
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
        commands.push(CommandInfo {
            name: file.name,
            description: frontmatter.description,
            agent: frontmatter.agent,
            model: frontmatter.model,
            source: "command",
            hints: command_hints(&template),
            template,
            subtask: frontmatter.subtask,
        });
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

fn command_hints(template: &str) -> Vec<String> {
    let mut numbered = Vec::new();
    let bytes = template.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'$' {
            let start = index;
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if index > start + 1 {
                let hint = &template[start..index];
                if !numbered.iter().any(|existing| existing == hint) {
                    numbered.push(hint.to_string());
                }
                continue;
            }
        }
        index += 1;
    }
    numbered.sort();
    if template.contains("$ARGUMENTS") {
        numbered.push("$ARGUMENTS".to_string());
    }
    numbered
}

fn add_skill_commands(commands: &mut Vec<CommandInfo>, workdir: &Path) {
    for skill in super::skill_catalog::list(workdir) {
        if commands.iter().any(|command| command.name == skill.name) {
            continue;
        }
        commands.push(CommandInfo {
            name: skill.name,
            description: Some(skill.description),
            agent: None,
            model: None,
            source: "skill",
            template: skill.content,
            hints: Vec::new(),
            subtask: None,
        });
    }
}
