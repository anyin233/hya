use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hya_legacy_tui::DialogItem;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandKind {
    Model,
    Resume,
    NewSession,
    Compact,
    Agent,
    Tools,
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
        name: "agent",
        aliases: &["agents"],
        description: "Select the active agent profile",
        key_hint: "tab",
        kind: CommandKind::Agent,
    },
    CommandSpec {
        name: "tools",
        aliases: &[],
        description: "Show builtin tools and MCP status",
        key_hint: "leader t",
        kind: CommandKind::Tools,
    },
    CommandSpec {
        name: "mcp",
        aliases: &[],
        description: "Show MCP and builtin tool status",
        key_hint: "leader t",
        kind: CommandKind::Tools,
    },
    CommandSpec {
        name: "think",
        aliases: &[],
        description: "Set reasoning effort for future turns",
        key_hint: "leader r",
        kind: CommandKind::Think,
    },
    CommandSpec {
        name: "export",
        aliases: &[],
        description: "Export the current transcript as Markdown",
        key_hint: "leader e",
        kind: CommandKind::Export,
    },
    CommandSpec {
        name: "quit",
        aliases: &["exit", "q"],
        description: "Exit hya",
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
    items.extend(
        custom
            .iter()
            .filter(|command| !shadows_builtin(command))
            .map(custom_command_item),
    );
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
            .filter(|command| command.name.starts_with(rest) && !shadows_builtin(command))
            .map(custom_command_item),
    );
    items
}

fn shadows_builtin(command: &CustomCommand) -> bool {
    resolve_slash(&command.name).is_some()
}

#[must_use]
pub fn find_custom<'a>(custom: &'a [CustomCommand], name: &str) -> Option<&'a CustomCommand> {
    custom.iter().find(|command| command.name == name)
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

#[cfg(test)]
pub fn load_skill_commands_from_dirs(dirs: &[PathBuf]) -> Vec<CustomCommand> {
    hya_tool::discover_skills_from_dirs(dirs)
        .into_iter()
        .map(|skill| CustomCommand {
            name: skill.name,
            description: skill.description,
            template: skill.content,
            agent: None,
            model: None,
        })
        .collect()
}

pub fn load_custom_commands(workdir: &Path) -> std::io::Result<Vec<CustomCommand>> {
    let mut commands = load_markdown_commands_from_dirs(&markdown_command_dirs(workdir))?;
    append_missing_skill_commands(&mut commands, load_skill_commands(workdir));
    Ok(commands)
}

pub fn load_skill_commands(workdir: &Path) -> Vec<CustomCommand> {
    hya_tool::discover_skills(workdir)
        .into_iter()
        .map(|skill| CustomCommand {
            name: skill.name,
            description: skill.description,
            template: skill.content,
            agent: None,
            model: None,
        })
        .collect()
}

#[cfg(test)]
pub fn load_custom_commands_from_dirs(
    markdown_dirs: &[PathBuf],
    skill_dirs: &[PathBuf],
) -> std::io::Result<Vec<CustomCommand>> {
    let mut commands = load_markdown_commands_from_dirs(markdown_dirs)?;
    append_missing_skill_commands(&mut commands, load_skill_commands_from_dirs(skill_dirs));
    Ok(commands)
}

fn append_missing_skill_commands(commands: &mut Vec<CustomCommand>, skills: Vec<CustomCommand>) {
    for skill in skills {
        if commands.iter().all(|command| command.name != skill.name) {
            commands.push(skill);
        }
    }
}

fn command_item(spec: &CommandSpec) -> DialogItem {
    DialogItem {
        label: format!("/{}", spec.name),
        detail: format!("{} · {}", spec.description, spec.key_hint),
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
        dirs.push(home.join(".config/hya/prompts"));
    }
    dirs.push(workdir.join(".opencode/commands"));
    dirs.push(workdir.join(".opencode/command"));
    dirs.push(workdir.join(".hya/prompts"));
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

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct HomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(home: &std::path::Path) -> Self {
            let lock = ENV_LOCK.lock().unwrap();
            let previous = std::env::var_os("HOME");
            unsafe {
                std::env::set_var("HOME", home);
            }
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(previous) = &self.previous {
                    std::env::set_var("HOME", previous);
                } else {
                    std::env::remove_var("HOME");
                }
            }
        }
    }

    fn write_skill(dir: &std::path::Path, name: &str, description: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn resolves_every_builtin_slash_command_and_alias() {
        for spec in COMMANDS {
            assert_eq!(resolve_slash(spec.name), Some(spec.kind), "{}", spec.name);
            for alias in spec.aliases {
                assert_eq!(resolve_slash(alias), Some(spec.kind), "alias {alias}");
            }
        }
        assert_eq!(resolve_slash("init"), None);
        assert_eq!(resolve_slash("yolo"), None);
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
        assert!(!items.iter().any(|item| item.label == "/init"));
        assert!(!items.iter().any(|item| item.label == "/yolo"));
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
        assert!(
            !completion_items("/")
                .iter()
                .any(|item| item.label == "/yolo")
        );
        assert!(completion_items("/model with args").is_empty());
        assert!(
            !completion_items("/in")
                .iter()
                .any(|item| item.label == "/init")
        );
        assert!(
            !completion_items("/yo")
                .iter()
                .any(|item| item.label == "/yolo")
        );
    }

    #[test]
    fn custom_commands_matching_builtin_names_are_hidden_from_help_and_completion() {
        let custom = vec![CustomCommand {
            name: "model".to_string(),
            description: "Unreachable custom model".to_string(),
            template: "custom".to_string(),
            agent: None,
            model: None,
        }];

        let help = help_items_with_custom(&custom);
        assert_eq!(help.iter().filter(|item| item.label == "/model").count(), 1);
        assert!(
            help.iter()
                .all(|item| !item.detail.contains("Unreachable custom model"))
        );

        let completion = completion_items_with_custom("/model", &custom);
        assert_eq!(
            completion
                .iter()
                .filter(|item| item.label == "/model")
                .count(),
            1
        );
        assert!(
            completion
                .iter()
                .all(|item| !item.detail.contains("Unreachable custom model"))
        );
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
    fn skill_commands_load_app_skill_content_as_template() {
        let root = temp_root();
        let skill_dir = root.join("skills").join("reviewer");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: reviewer\ndescription: Review code carefully\n---\nUse this skill on $ARGUMENTS.\n",
        )
        .unwrap();

        let commands = load_skill_commands_from_dirs(&[root.join("skills")]);

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "reviewer");
        assert_eq!(commands[0].description, "Review code carefully");
        assert_eq!(
            commands[0].expand("src/main.rs"),
            "Use this skill on src/main.rs.\n"
        );
    }

    #[test]
    fn markdown_commands_override_skill_commands_with_same_name() {
        let root = temp_root();
        let prompt_dir = root.join("prompts");
        let skill_root = root.join("skills");
        let skill_dir = skill_root.join("reviewer");
        std::fs::create_dir_all(&prompt_dir).unwrap();
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            prompt_dir.join("reviewer.md"),
            "---\ndescription: Prompt reviewer\n---\nPrompt $ARGUMENTS\n",
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: reviewer\ndescription: Skill reviewer\n---\nSkill $ARGUMENTS\n",
        )
        .unwrap();

        let commands = load_custom_commands_from_dirs(&[prompt_dir], &[skill_root]).unwrap();

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].description, "Prompt reviewer");
        assert_eq!(commands[0].expand("file"), "Prompt file\n");
    }

    #[test]
    fn skill_commands_preserve_catalog_order_and_first_winner() {
        let root = temp_root();
        let home = temp_root();
        let _home = HomeGuard::set(&home);
        write_skill(
            &root.join(".hya/skills/z-local"),
            "z-local",
            "Local Z",
            "Local Z $ARGUMENTS\n",
        );
        write_skill(
            &root.join(".hya/skills/shared"),
            "shared",
            "Local",
            "Local $ARGUMENTS\n",
        );
        write_skill(
            &home.join(".config/hya/skills/a-home"),
            "a-home",
            "Home A",
            "Home A $ARGUMENTS\n",
        );
        write_skill(
            &home.join(".config/hya/skills/shared"),
            "shared",
            "Home",
            "Home $ARGUMENTS\n",
        );

        let commands = load_custom_commands(&root).unwrap();

        let names = commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>();
        let z_local = names.iter().position(|name| *name == "z-local").unwrap();
        let a_home = names.iter().position(|name| *name == "a-home").unwrap();
        assert!(
            z_local < a_home,
            "catalog order should beat alphabetical order: {names:?}"
        );
        let shared = commands
            .iter()
            .find(|command| command.name == "shared")
            .unwrap();
        assert_eq!(shared.description, "Local");
        assert_eq!(shared.expand("file"), "Local file\n");
        let completion = completion_items_with_custom("/", &commands);
        let completion_z = completion
            .iter()
            .position(|item| item.label == "/z-local")
            .unwrap();
        let completion_a = completion
            .iter()
            .position(|item| item.label == "/a-home")
            .unwrap();
        assert!(
            completion_z < completion_a,
            "custom completion order should preserve catalog order: {completion:?}"
        );
        let help = help_items_with_custom(&commands);
        let help_z = help
            .iter()
            .position(|item| item.label == "/z-local")
            .unwrap();
        let help_a = help
            .iter()
            .position(|item| item.label == "/a-home")
            .unwrap();
        assert!(
            help_z < help_a,
            "custom help order should preserve catalog order: {help:?}"
        );
        assert!(completion.iter().any(|item| item.label == "/shared"));
        assert!(help.iter().any(|item| item.label == "/shared"));
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
        std::env::temp_dir().join(format!("hya-command-test-{nanos}-{}", std::process::id()))
    }
}
