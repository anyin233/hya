use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use yaca_tui::DialogItem;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomCommand {
    pub name: String,
    pub description: String,
    pub template: String,
    pub agent: Option<String>,
    pub model: Option<String>,
}

impl CustomCommand {
    #[must_use]
    pub fn expand(&self, arguments: &str) -> String {
        let mut out = self.template.replace("$ARGUMENTS", arguments);
        let positional = split_arguments(arguments);
        for idx in 1..=9 {
            let needle = format!("${idx}");
            let replacement = positional.get(idx - 1).cloned().unwrap_or_default();
            out = out.replace(&needle, &replacement);
        }
        out
    }
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

#[must_use]
pub fn find_custom<'a>(custom: &'a [CustomCommand], name: &str) -> Option<&'a CustomCommand> {
    custom.iter().find(|command| command.name == name)
}

pub fn load_markdown_commands(workdir: &Path) -> std::io::Result<Vec<CustomCommand>> {
    load_markdown_commands_from_dirs(&markdown_command_dirs(workdir))
}

pub fn load_markdown_commands_from_dirs(dirs: &[PathBuf]) -> std::io::Result<Vec<CustomCommand>> {
    let mut commands = BTreeMap::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let text = std::fs::read_to_string(&path)?;
            let command = parse_markdown_command(name, &text);
            commands.insert(command.name.clone(), command);
        }
    }
    Ok(commands.into_values().collect())
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

fn markdown_command_dirs(workdir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        let config = home.join(".config/opencode");
        dirs.push(config.join("commands"));
        dirs.push(config.join("command"));
        dirs.push(home.join(".config/yaca/prompts"));
    }
    dirs.push(workdir.join(".opencode/commands"));
    dirs.push(workdir.join(".opencode/command"));
    dirs.push(workdir.join(".yaca/prompts"));
    dirs
}

fn parse_markdown_command(name: &str, text: &str) -> CustomCommand {
    let (frontmatter, body) = split_frontmatter(text);
    let description = frontmatter
        .get("description")
        .cloned()
        .unwrap_or_else(|| format!("Run custom command /{name}"));
    CustomCommand {
        name: name.to_string(),
        description,
        template: body.to_string(),
        agent: frontmatter.get("agent").cloned(),
        model: frontmatter.get("model").cloned(),
    }
}

fn split_frontmatter(text: &str) -> (BTreeMap<String, String>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (BTreeMap::new(), text);
    };
    let Some(end) = rest.find("\n---\n") else {
        return (BTreeMap::new(), text);
    };
    let frontmatter_text = &rest[..end];
    let body = &rest[end + "\n---\n".len()..];
    let mut frontmatter = BTreeMap::new();
    for line in frontmatter_text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        frontmatter.insert(
            key.trim().to_string(),
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    (frontmatter, body)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn resolves_slash_commands_and_aliases() {
        assert_eq!(resolve_slash("model"), Some(CommandKind::Model));
        assert_eq!(resolve_slash("models"), Some(CommandKind::Model));
        assert_eq!(resolve_slash("resume"), Some(CommandKind::Resume));
        assert_eq!(resolve_slash("sessions"), Some(CommandKind::Resume));
        assert_eq!(resolve_slash("new"), Some(CommandKind::NewSession));
        assert_eq!(resolve_slash("clear"), Some(CommandKind::NewSession));
        assert_eq!(resolve_slash("compact"), Some(CommandKind::Compact));
        assert_eq!(resolve_slash("init"), Some(CommandKind::Init));
        assert_eq!(resolve_slash("agent"), Some(CommandKind::Agent));
        assert_eq!(resolve_slash("tools"), Some(CommandKind::Tools));
        assert_eq!(resolve_slash("mcp"), Some(CommandKind::Tools));
        assert_eq!(resolve_slash("yolo"), Some(CommandKind::Yolo));
        assert_eq!(resolve_slash("think"), Some(CommandKind::Think));
        assert_eq!(resolve_slash("export"), Some(CommandKind::Export));
        assert_eq!(resolve_slash("quit"), Some(CommandKind::Quit));
        assert_eq!(resolve_slash("exit"), Some(CommandKind::Quit));
        assert_eq!(resolve_slash("q"), Some(CommandKind::Quit));
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
        assert!(items.iter().any(|item| item.label == "/export"));
        assert!(items.iter().any(|item| item.label == "/quit"));
        assert!(items.iter().any(|item| item.label == "/help"));
    }

    #[test]
    fn status_commands_advertise_opencode_leader_shortcut() {
        let items = help_items();

        let tools_detail = items
            .iter()
            .find(|item| item.label == "/tools")
            .map(|item| item.detail.as_str());
        let mcp_detail = items
            .iter()
            .find(|item| item.label == "/mcp")
            .map(|item| item.detail.as_str());

        assert!(matches!(tools_detail, Some(detail) if detail.starts_with("leader s")));
        assert!(matches!(mcp_detail, Some(detail) if detail.starts_with("leader s")));
    }

    #[test]
    fn export_command_advertises_opencode_session_export_shortcut() {
        let items = help_items();

        let export_detail = items
            .iter()
            .find(|item| item.label == "/export")
            .map(|item| item.detail.as_str());

        assert!(matches!(export_detail, Some(detail) if detail.starts_with("leader x")));
    }

    #[test]
    fn command_help_avoids_unimplemented_leader_shortcuts() {
        let items = help_items();

        let detail = |label: &str| {
            items
                .iter()
                .find(|item| item.label == label)
                .map(|item| item.detail.as_str())
        };

        assert!(matches!(detail("/agent"), Some(text) if text.starts_with("leader a")));
        assert!(matches!(detail("/init"), Some(text) if text.starts_with("/init")));
        assert!(matches!(detail("/think"), Some(text) if text.starts_with("/think")));
    }

    #[test]
    fn completion_items_filter_by_prefix() {
        let items = completion_items("/mo");

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "/model");
        assert!(
            completion_items("/")
                .iter()
                .any(|item| item.label == "/resume")
        );
        assert!(completion_items("/model with args").is_empty());
    }

    #[test]
    fn markdown_commands_load_frontmatter_and_expand_arguments() {
        let root = temp_root();
        let commands_dir = root.join(".opencode").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::write(
            commands_dir.join("component.md"),
            r#"---
description: Create a component
agent: build
model: anthropic/claude-sonnet
---
Create $1 in $2.

All args: $ARGUMENTS
"#,
        )
        .unwrap();

        let commands = load_markdown_commands_from_dirs(&[commands_dir]).unwrap();

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "component");
        assert_eq!(commands[0].description, "Create a component");
        assert_eq!(commands[0].agent.as_deref(), Some("build"));
        assert_eq!(
            commands[0].model.as_deref(),
            Some("anthropic/claude-sonnet")
        );
        assert_eq!(
            commands[0].expand("Button src/components"),
            "Create Button in src/components.\n\nAll args: Button src/components\n"
        );
    }

    #[test]
    fn custom_commands_appear_in_completion_items() {
        let custom = vec![CustomCommand {
            name: "test".to_string(),
            description: "Run tests".to_string(),
            template: "Run $ARGUMENTS".to_string(),
            agent: None,
            model: None,
        }];

        let items = completion_items_with_custom("/t", &custom);

        let detail = items
            .iter()
            .find(|item| item.label == "/test")
            .map(|item| item.detail.as_str());
        assert!(matches!(detail, Some(detail) if detail.contains("Run tests")));
    }

    fn temp_root() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("yaca-command-test-{nanos}-{}", std::process::id()))
    }
}
