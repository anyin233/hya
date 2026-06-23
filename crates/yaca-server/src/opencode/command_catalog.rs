use std::path::{Path, PathBuf};

use serde::Serialize;

const INIT_TEMPLATE: &str = include_str!("command_templates/initialize.txt");
const REVIEW_TEMPLATE: &str = include_str!("command_templates/review.txt");

#[derive(Serialize)]
pub(in crate::opencode) struct CommandInfo {
    name: String,
    description: String,
    source: &'static str,
    template: String,
    hints: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
        description: description.into(),
        source: "command",
        template,
        hints,
        subtask,
    }
}

fn add_skill_commands(commands: &mut Vec<CommandInfo>, workdir: &Path) {
    for (name, description, template) in discover_skill_commands(workdir) {
        if commands.iter().any(|command| command.name == name) {
            continue;
        }
        commands.push(CommandInfo {
            name,
            description,
            source: "skill",
            template,
            hints: Vec::new(),
            subtask: None,
        });
    }
}

fn discover_skill_commands(workdir: &Path) -> Vec<(String, String, String)> {
    let mut skills = Vec::new();
    for dir in skill_dirs(workdir) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path().join("SKILL.md");
            let Ok(content) = std::fs::read_to_string(path) else {
                continue;
            };
            if let Some(skill) = parse_skill(&content) {
                skills.push(skill);
            }
        }
    }
    skills.sort_by(|a, b| a.0.cmp(&b.0));
    skills
}

fn parse_skill(content: &str) -> Option<(String, String, String)> {
    let (frontmatter, body) = content.strip_prefix("---")?.split_once("\n---")?;
    let mut name = None;
    let mut description = None;
    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("name:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("description:") {
            description = Some(value.trim().to_string());
        }
    }
    Some((
        name?,
        description?,
        body.strip_prefix('\n').unwrap_or(body).to_string(),
    ))
}

fn skill_dirs(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![workdir.join(".yaca/skills")];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".config/yaca/skills"));
    }
    dirs
}
