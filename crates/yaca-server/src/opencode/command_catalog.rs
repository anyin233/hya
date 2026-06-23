use std::path::Path;

use serde::Serialize;

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
    upsert_commands(
        &mut commands,
        super::command_sources::config_commands(Path::new(workdir.as_ref())),
    );
    upsert_commands(
        &mut commands,
        super::command_sources::disk_commands(Path::new(workdir.as_ref())),
    );
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

impl CommandInfo {
    pub(super) fn command(
        name: String,
        description: Option<String>,
        agent: Option<String>,
        model: Option<String>,
        template: String,
        subtask: Option<bool>,
    ) -> Self {
        Self {
            name,
            description,
            agent,
            model,
            source: "command",
            hints: super::command_sources::command_hints(&template),
            template,
            subtask,
        }
    }

    fn skill(name: String, description: String, template: String) -> Self {
        Self {
            name,
            description: Some(description),
            agent: None,
            model: None,
            source: "skill",
            template,
            hints: Vec::new(),
            subtask: None,
        }
    }
}

fn upsert_commands(commands: &mut Vec<CommandInfo>, incoming: Vec<CommandInfo>) {
    for command in incoming {
        if let Some(existing) = commands.iter_mut().find(|item| item.name == command.name) {
            *existing = command;
        } else {
            commands.push(command);
        }
    }
}

fn add_skill_commands(commands: &mut Vec<CommandInfo>, workdir: &Path) {
    for skill in super::skill_catalog::list(workdir) {
        if commands.iter().any(|command| command.name == skill.name) {
            continue;
        }
        commands.push(CommandInfo::skill(
            skill.name,
            skill.description,
            skill.content,
        ));
    }
}
