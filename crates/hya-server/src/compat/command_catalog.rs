use std::path::Path;

use serde::Serialize;

const INIT_TEMPLATE: &str = include_str!("command_templates/initialize.txt");
const REVIEW_TEMPLATE: &str = include_str!("command_templates/review.txt");

#[derive(Serialize)]
pub(in crate::compat) struct CommandInfo {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    source: &'static str,
    #[serde(skip)]
    expandable: bool,
    template: String,
    hints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtask: Option<bool>,
}

pub(in crate::compat) fn list(workdir: &Path) -> Vec<CommandInfo> {
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

pub(in crate::compat) fn expand_prompt(
    workdir: &Path,
    command: &str,
    arguments: &str,
) -> Option<String> {
    list(workdir)
        .into_iter()
        .find(|item| item.name == command && item.expandable)
        .map(|item| expand_template(&item.template, arguments))
}

fn expand_template(template: &str, arguments: &str) -> String {
    let positional = split_arguments(arguments);
    let mut out = String::with_capacity(template.len().saturating_add(arguments.len()));
    let mut chars = template.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch != '$' {
            out.push(ch);
            continue;
        }
        if template[idx..].starts_with("$ARGUMENTS") {
            out.push_str(arguments);
            for _ in 0.."ARGUMENTS".len() {
                chars.next();
            }
            continue;
        }
        let mut position = 0usize;
        let mut has_digits = false;
        while let Some((_, next)) = chars.peek().copied() {
            if let Some(digit) = next.to_digit(10) {
                has_digits = true;
                position = position.saturating_mul(10).saturating_add(digit as usize);
                chars.next();
            } else {
                break;
            }
        }
        if has_digits {
            if let Some(replacement) = position.checked_sub(1).and_then(|idx| positional.get(idx)) {
                out.push_str(replacement);
            }
        } else {
            out.push('$');
        }
    }
    out
}

fn split_arguments(arguments: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in arguments.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(ch),
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
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
        expandable: false,
        template,
        hints: hints.into_iter().map(str::to_string).collect(),
        subtask,
    }
}

impl CommandInfo {
    /// Wire form for TUI bootstrap: omits heavy `template` bodies (expand is server-side).
    #[must_use]
    pub(super) fn bootstrap_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "agent": self.agent,
            "model": self.model,
            "source": self.source,
            "hints": self.hints,
            "subtask": self.subtask,
        })
    }

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
            expandable: true,
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
            expandable: true,
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

#[cfg(test)]
mod tests {
    use super::expand_template;

    #[test]
    fn expands_full_numeric_placeholder_without_reexpanding_arguments() {
        let arguments = "one two three four five six seven eight nine-literal-$1 ten";

        assert_eq!(
            expand_template("first=$1 tenth=$10 missing=$11 all=$ARGUMENTS", arguments),
            "first=one tenth=ten missing= all=one two three four five six seven eight nine-literal-$1 ten"
        );
    }
}
