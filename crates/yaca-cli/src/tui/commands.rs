use yaca_tui::DialogItem;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandKind {
    Model,
    Resume,
    NewSession,
    Help,
}

pub struct CommandSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub key_hint: &'static str,
    pub kind: CommandKind,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "model",
        aliases: &["models"],
        description: "Select the model for the next assistant turn",
        key_hint: "leader m",
        kind: CommandKind::Model,
    },
    CommandSpec {
        name: "resume",
        aliases: &[],
        description: "Resume a previous conversation",
        key_hint: "leader l",
        kind: CommandKind::Resume,
    },
    CommandSpec {
        name: "new",
        aliases: &[],
        description: "Start a new conversation",
        key_hint: "leader n",
        kind: CommandKind::NewSession,
    },
    CommandSpec {
        name: "help",
        aliases: &["?"],
        description: "Show commands and shortcuts",
        key_hint: "?",
        kind: CommandKind::Help,
    },
];

#[must_use]
pub fn resolve_slash(input: &str) -> Option<CommandKind> {
    let command = input.split_whitespace().next().unwrap_or_default();
    COMMANDS.iter().find_map(|spec| {
        (spec.name == command || spec.aliases.contains(&command)).then_some(spec.kind)
    })
}

#[must_use]
pub fn help_items() -> Vec<DialogItem> {
    COMMANDS.iter().map(command_item).collect()
}

#[must_use]
pub fn completion_items(input: &str) -> Vec<DialogItem> {
    let Some(rest) = input.strip_prefix('/') else {
        return Vec::new();
    };
    if rest.contains(char::is_whitespace) {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|spec| {
            spec.name.starts_with(rest) || spec.aliases.iter().any(|alias| alias.starts_with(rest))
        })
        .map(command_item)
        .collect()
}

fn command_item(spec: &CommandSpec) -> DialogItem {
    DialogItem {
        label: format!("/{}", spec.name),
        detail: format!("{} · {}", spec.description, spec.key_hint),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_slash_commands_and_aliases() {
        assert_eq!(resolve_slash("model"), Some(CommandKind::Model));
        assert_eq!(resolve_slash("models"), Some(CommandKind::Model));
        assert_eq!(resolve_slash("resume"), Some(CommandKind::Resume));
        assert_eq!(resolve_slash("new"), Some(CommandKind::NewSession));
        assert_eq!(resolve_slash("help"), Some(CommandKind::Help));
    }

    #[test]
    fn unknown_slash_command_is_not_resolved() {
        assert_eq!(resolve_slash("nope"), None);
    }

    #[test]
    fn help_items_come_from_registered_commands() {
        let items = help_items();
        assert!(items.iter().any(|item| item.label == "/model"));
        assert!(items.iter().any(|item| item.label == "/resume"));
        assert!(items.iter().any(|item| item.label == "/new"));
        assert!(items.iter().any(|item| item.label == "/help"));
    }

    #[test]
    fn completion_items_filter_by_prefix() {
        let items = completion_items("/m");

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "/model");
        assert!(
            completion_items("/")
                .iter()
                .any(|item| item.label == "/resume")
        );
        assert!(completion_items("/model with args").is_empty());
    }
}
