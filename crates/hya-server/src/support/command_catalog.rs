use std::path::Path;

use serde::{Deserialize, Serialize};

/// Identity of the trusted bundle that owns the prompt-template commands.
const CORE_COMMANDS_BUNDLE: &str = "hya/core-commands";

/// The `commands.yaml` asset of `hya/core-commands`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreCommands {
    schema_version: u32,
    commands: Vec<CoreCommand>,
}

/// One prompt-template command declared by `hya/core-commands`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreCommand {
    name: String,
    description: String,
    /// `extensions.files` id of the template body.
    template: String,
    #[serde(default)]
    hints: Vec<String>,
    #[serde(default)]
    subtask: Option<bool>,
}

/// Prompt-template commands from `hya/core-commands`, loaded once per process.
fn core_commands() -> &'static [(CoreCommand, String)] {
    static COMMANDS: std::sync::OnceLock<Vec<(CoreCommand, String)>> = std::sync::OnceLock::new();
    COMMANDS.get_or_init(|| {
        load_core_commands().unwrap_or_else(|error| panic!("load {CORE_COMMANDS_BUNDLE}: {error}"))
    })
}

fn load_core_commands() -> Result<Vec<(CoreCommand, String)>, String> {
    let catalog =
        hya_bundle::first_party_bundle(CORE_COMMANDS_BUNDLE).map_err(|error| error.to_string())?;
    let [bundle] = catalog.bundles() else {
        return Err("expected one bundle".to_string());
    };
    let asset = |id: &str| {
        bundle
            .extensions()
            .iter()
            .find(|asset| asset.local_id == id)
            .map(|asset| asset.content.clone())
            .ok_or_else(|| format!("missing `{id}` file"))
    };
    let declared: CoreCommands =
        serde_norway::from_str(&asset("commands")?).map_err(|error| error.to_string())?;
    if declared.schema_version != 1 {
        return Err(format!(
            "unsupported commands schema {}",
            declared.schema_version
        ));
    }
    declared
        .commands
        .into_iter()
        .map(|command| {
            let template = asset(&command.template)?;
            Ok((command, template))
        })
        .collect()
}

#[derive(Serialize)]
pub(crate) struct CommandInfo {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,

    pub(crate) source: &'static str,
    #[serde(skip)]
    expandable: bool,
    pub(crate) template: String,

    pub(crate) hints: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) subtask: Option<bool>,
}

pub(crate) fn list(workdir: &Path) -> Vec<CommandInfo> {
    let workdir = workdir.to_string_lossy();
    let mut commands = core_commands()
        .iter()
        .map(|(command, template)| CommandInfo {
            hints: command.hints.clone(),
            ..command_info(
                command.name.clone(),
                command.description.clone(),
                template.replace("${path}", workdir.as_ref()),
                Vec::new(),
                command.subtask,
            )
        })
        .collect::<Vec<_>>();
    commands.extend([
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
        command_info(
            "workflow",
            "inspect or run workflows",
            "/workflow $ARGUMENTS".to_string(),
            vec!["$ARGUMENTS"],
            None,
        ),
    ]);
    upsert_commands(
        &mut commands,
        crate::support::command_sources::disk_commands(Path::new(workdir.as_ref())),
    );
    add_skill_commands(&mut commands, Path::new(workdir.as_ref()));
    commands
}

pub(crate) fn expand_prompt(workdir: &Path, command: &str, arguments: &str) -> Option<String> {
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
            hints: command_hints(&template),
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
    for skill in crate::support::skill_catalog::list(workdir) {
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
    fn init_and_review_templates_come_from_the_core_commands_bundle() {
        let catalog = hya_bundle::first_party_bundle("hya/core-commands")
            .unwrap_or_else(|error| panic!("{error}"));
        let asset = |id: &str| {
            catalog.bundles()[0]
                .extensions()
                .iter()
                .find(|asset| asset.local_id == id)
                .map(|asset| asset.content.replace("${path}", "/work"))
                .unwrap_or_else(|| panic!("missing {id} asset"))
        };
        let commands = super::list(std::path::Path::new("/work"));
        for (name, id, subtask) in [("init", "init", None), ("review", "review", Some(true))] {
            let command = commands
                .iter()
                .find(|command| command.name == name)
                .unwrap_or_else(|| panic!("missing /{name}"));
            assert_eq!(command.template, asset(id), "/{name}");
            assert_eq!(command.subtask, subtask, "/{name}");
        }
    }

    #[test]
    fn expands_full_numeric_placeholder_without_reexpanding_arguments() {
        let arguments = "one two three four five six seven eight nine-literal-$1 ten";

        assert_eq!(
            expand_template("first=$1 tenth=$10 missing=$11 all=$ARGUMENTS", arguments),
            "first=one tenth=ten missing= all=one two three four five six seven eight nine-literal-$1 ten"
        );
    }
}
