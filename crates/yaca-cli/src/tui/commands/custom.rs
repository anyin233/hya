use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
