use yaca_tui::DialogItem;

mod custom;

pub use custom::{
    CustomCommand, find_custom, load_markdown_commands, load_markdown_commands_from_dirs,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandKind {
    Model,
    Resume,
    NewSession,
    Compact,
    Init,
    Agent,
    Tools,
    Yolo,
    Think,
    Export,
    Quit,
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
        aliases: &["sessions"],
        description: "Resume a previous conversation",
        key_hint: "leader l",
        kind: CommandKind::Resume,
    },
    CommandSpec {
        name: "new",
        aliases: &["clear"],
        description: "Start a new conversation",
        key_hint: "leader n",
        kind: CommandKind::NewSession,
    },
    CommandSpec {
        name: "compact",
        aliases: &[],
        description: "Compact prior conversation context",
        key_hint: "leader c",
        kind: CommandKind::Compact,
    },
    CommandSpec {
        name: "init",
        aliases: &[],
        description: "Create AGENTS.md project instructions",
        key_hint: "/init",
        kind: CommandKind::Init,
    },
    CommandSpec {
        name: "agent",
        aliases: &["agents"],
        description: "Select the active agent profile",
        key_hint: "leader a",
        kind: CommandKind::Agent,
    },
    CommandSpec {
        name: "tools",
        aliases: &[],
        description: "Show builtin tools and MCP status",
        key_hint: "leader s",
        kind: CommandKind::Tools,
    },
    CommandSpec {
        name: "mcp",
        aliases: &[],
        description: "Show MCP and builtin tool status",
        key_hint: "leader s",
        kind: CommandKind::Tools,
    },
    CommandSpec {
        name: "yolo",
        aliases: &[],
        description: "Toggle or set auto-approve mode",
        key_hint: "/yolo",
        kind: CommandKind::Yolo,
    },
    CommandSpec {
        name: "think",
        aliases: &[],
        description: "Set reasoning effort for future turns",
        key_hint: "/think",
        kind: CommandKind::Think,
    },
    CommandSpec {
        name: "export",
        aliases: &[],
        description: "Export the current transcript as Markdown",
        key_hint: "leader x",
        kind: CommandKind::Export,
    },
    CommandSpec {
        name: "quit",
        aliases: &["exit", "q"],
        description: "Exit yaca",
        key_hint: "ctrl-c ctrl-c",
        kind: CommandKind::Quit,
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

#[must_use]
pub fn help_items_with_custom(custom: &[CustomCommand]) -> Vec<DialogItem> {
    let mut items = help_items();
    items.extend(custom.iter().map(custom_command_item));
    items
}

#[must_use]
pub fn completion_items_with_custom(input: &str, custom: &[CustomCommand]) -> Vec<DialogItem> {
    let mut items = completion_items(input);
    let Some(rest) = input.strip_prefix('/') else {
        return items;
    };
    if rest.contains(char::is_whitespace) {
        return items;
    }
    items.extend(
        custom
            .iter()
            .filter(|command| command.name.starts_with(rest))
            .map(custom_command_item),
    );
    items
}

fn command_item(spec: &CommandSpec) -> DialogItem {
    DialogItem {
        label: format!("/{}", spec.name),
        detail: format!("{} · {}", spec.key_hint, spec.description),
    }
}

fn custom_command_item(command: &CustomCommand) -> DialogItem {
    DialogItem {
        label: format!("/{}", command.name),
        detail: format!("{} · custom", command.description),
    }
}

#[cfg(test)]
mod tests;
