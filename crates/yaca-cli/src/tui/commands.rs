use yaca_tui::DialogItem;

mod custom;
mod registry;

pub use custom::{CustomCommand, find_custom, load_markdown_commands};
pub use registry::CommandKind;

use registry::{COMMANDS, CommandSpec};

#[must_use]
pub fn resolve_slash(input: &str) -> Option<CommandKind> {
    let command = input.split_whitespace().next().unwrap_or_default();
    COMMANDS.iter().find_map(|spec| {
        (spec.name == command || spec.aliases.contains(&command)).then_some(spec.kind)
    })
}

#[must_use]
pub fn help_items() -> Vec<DialogItem> {
    grouped_command_specs()
        .into_iter()
        .map(command_item)
        .collect()
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
pub fn palette_items_with_custom(custom: &[CustomCommand]) -> Vec<DialogItem> {
    let mut items = COMMANDS
        .iter()
        .filter(|spec| spec.suggested)
        .map(suggested_command_item)
        .collect::<Vec<_>>();
    items.extend(help_items_with_custom(custom));
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
        detail: format!(
            "{} · {} · {}",
            spec.category.label(),
            spec.key_hint,
            spec.description
        ),
    }
}

fn grouped_command_specs() -> Vec<&'static CommandSpec> {
    let mut categories = Vec::new();
    for spec in COMMANDS {
        if !categories.contains(&spec.category) {
            categories.push(spec.category);
        }
    }
    let mut specs = Vec::new();
    for category in categories {
        specs.extend(
            COMMANDS
                .iter()
                .filter(move |spec| spec.category == category),
        );
    }
    specs
}

fn suggested_command_item(spec: &CommandSpec) -> DialogItem {
    DialogItem {
        label: format!("/{}", spec.name),
        detail: format!("Suggested · {} · {}", spec.key_hint, spec.description),
    }
}

fn custom_command_item(command: &CustomCommand) -> DialogItem {
    DialogItem {
        label: format!("/{}", command.name),
        detail: format!("Custom · {} · custom", command.description),
    }
}

#[cfg(test)]
mod tests;
