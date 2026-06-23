use std::path::Path;

use serde::Serialize;

const INIT_TEMPLATE: &str = include_str!("command_templates/initialize.txt");
const REVIEW_TEMPLATE: &str = include_str!("command_templates/review.txt");

#[derive(Serialize)]
pub(in crate::opencode) struct CommandInfo {
    name: &'static str,
    description: &'static str,
    source: &'static str,
    template: String,
    hints: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtask: Option<bool>,
}

pub(in crate::opencode) fn list(workdir: &Path) -> Vec<CommandInfo> {
    let workdir = workdir.to_string_lossy();
    vec![
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
    ]
}

fn command_info(
    name: &'static str,
    description: &'static str,
    template: String,
    hints: Vec<&'static str>,
    subtask: Option<bool>,
) -> CommandInfo {
    CommandInfo {
        name,
        description,
        source: "command",
        template,
        hints,
        subtask,
    }
}
