//! Bundle-contributed `hya` CLI commands (`cli:` in `bundle.yaml`, ADR-0040).
//!
//! A bundle declares commands with their arguments; the `hya` binary adds
//! them to its command line when the bundle is enabled and routes each
//! invocation to the bundle's driver or BundleApi endpoint.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::BundleError;

/// One CLI contribution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CliCommand {
    /// `command` adds `hya <name>`; `prompt` claims `hya -p/--prompt` and adds
    /// its arguments as top-level flags.
    #[serde(default, skip_serializing_if = "CliCommandKind::is_command")]
    pub kind: CliCommandKind,
    /// Subcommand name (`kind: command` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// One-line help text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub about: String,
    /// Route: this bundle's driver of that name (`<bundle-id>:<driver>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    /// Route: this bundle's BundleApi endpoint of that id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// Arguments, in help order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<CliArg>,
    /// Nested subcommands; a command with subcommands has no route itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcommands: Vec<CliCommand>,
}

/// What a [`CliCommand`] adds to the command line.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CliCommandKind {
    /// `hya <name> …`.
    #[default]
    Command,
    /// `hya -p <PROMPT> …`.
    Prompt,
}

impl CliCommandKind {
    fn is_command(&self) -> bool {
        *self == Self::Command
    }
}

/// One argument of a [`CliCommand`]. Without `long` or `short` it is positional.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CliArg {
    /// Key of the parsed value in the route's JSON arguments.
    pub id: String,
    /// `--long` flag name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long: Option<String>,
    /// `-s` flag letter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short: Option<char>,
    /// Value type.
    #[serde(default, skip_serializing_if = "CliValue::is_string")]
    pub value: CliValue,
    /// Whether the argument must be given.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// Default value, as typed on the command line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Help text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub help: String,
    /// Placeholder shown in help (`<VALUE_NAME>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_name: Option<String>,
    /// Extra `--long` names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Ids of arguments that may not be given together with this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts_with: Vec<String>,
}

/// JSON type of a parsed [`CliArg`].
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CliValue {
    /// A string.
    #[default]
    String,
    /// An unsigned integer.
    Integer,
    /// A boolean switch that takes no value.
    Flag,
    /// A repeatable string (`--x a --x b` → `["a", "b"]`).
    List,
}

impl CliValue {
    fn is_string(&self) -> bool {
        *self == Self::String
    }
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && name.len() <= 64
}

/// Validate a bundle's `cli:` section.
///
/// # Errors
/// [`BundleError::InvalidManifest`] on a malformed contribution.
pub(crate) fn validate_cli(
    bundle_id: &str,
    commands: &[CliCommand],
    api_ids: &BTreeSet<&str>,
    native: bool,
) -> Result<(), BundleError> {
    let invalid = |detail: String| BundleError::InvalidManifest {
        source_name: bundle_id.to_string(),
        detail,
    };
    let mut names = BTreeSet::new();
    let mut prompts = 0;
    for command in commands {
        match command.kind {
            CliCommandKind::Prompt => {
                prompts += 1;
                if command.name.is_some() || !command.subcommands.is_empty() {
                    return Err(invalid(
                        "a `kind: prompt` cli entry has no name and no subcommands".into(),
                    ));
                }
            }
            CliCommandKind::Command => {
                let name = command.name.as_deref().unwrap_or_default();
                if !names.insert(name) {
                    return Err(invalid(format!("duplicate cli command `{name}`")));
                }
            }
        }
        validate_command(command, api_ids, native, &invalid, true)?;
    }
    if prompts > 1 {
        return Err(invalid("at most one `kind: prompt` cli entry".into()));
    }
    Ok(())
}

fn validate_command(
    command: &CliCommand,
    api_ids: &BTreeSet<&str>,
    native: bool,
    invalid: &dyn Fn(String) -> BundleError,
    top: bool,
) -> Result<(), BundleError> {
    let label = command.name.as_deref().unwrap_or("-p");
    if command.kind == CliCommandKind::Command && !command.name.as_deref().is_some_and(valid_name) {
        return Err(invalid(format!(
            "cli command name `{label}` must match `[a-z][a-z0-9-]*`"
        )));
    }
    if !top && command.kind == CliCommandKind::Prompt {
        return Err(invalid(
            "`kind: prompt` is only valid at the top level".into(),
        ));
    }
    let routes = usize::from(command.driver.is_some()) + usize::from(command.api.is_some());
    match (command.subcommands.is_empty(), routes) {
        (true, 1) | (false, 0) => {}
        (true, _) => {
            return Err(invalid(format!(
                "cli command `{label}` needs exactly one of `driver` or `api`"
            )));
        }
        (false, _) => {
            return Err(invalid(format!(
                "cli command `{label}` has subcommands, so it has no route of its own"
            )));
        }
    }
    if command.driver.is_some() && !native {
        return Err(invalid(format!(
            "cli command `{label}` routes to a driver, which needs `runtime.kind: native`"
        )));
    }
    if let Some(api) = &command.api
        && !native
        && !api_ids.contains(api.as_str())
    {
        return Err(invalid(format!(
            "cli command `{label}` routes to undeclared api `{api}`"
        )));
    }
    let mut ids = BTreeSet::new();
    let mut longs = BTreeSet::new();
    for arg in &command.args {
        if !valid_name(&arg.id.replace('_', "-")) || !ids.insert(arg.id.as_str()) {
            return Err(invalid(format!(
                "cli command `{label}`: invalid or duplicate argument id `{}`",
                arg.id
            )));
        }
        for long in arg.long.iter().chain(&arg.aliases) {
            if !valid_name(long) || !longs.insert(long.as_str()) {
                return Err(invalid(format!(
                    "cli command `{label}`: invalid or duplicate flag `--{long}`"
                )));
            }
        }
        if arg.value == CliValue::Flag && (arg.default.is_some() || arg.required) {
            return Err(invalid(format!(
                "cli command `{label}`: flag `{}` takes no default and is never required",
                arg.id
            )));
        }
    }
    for arg in &command.args {
        if let Some(other) = arg
            .conflicts_with
            .iter()
            .find(|other| !ids.contains(other.as_str()))
        {
            return Err(invalid(format!(
                "cli command `{label}`: `{}` conflicts with unknown argument `{other}`",
                arg.id
            )));
        }
    }
    let mut sub_names = BTreeSet::new();
    for sub in &command.subcommands {
        if !sub_names.insert(sub.name.as_deref().unwrap_or_default()) {
            return Err(invalid(format!(
                "cli command `{label}` has duplicate subcommands"
            )));
        }
        validate_command(sub, api_ids, native, invalid, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn parse(yaml: &str) -> Vec<CliCommand> {
        serde_norway::from_str(yaml).unwrap()
    }

    #[test]
    fn accepts_commands_prompt_and_nested_subcommands() {
        let commands = parse(
            r#"
- name: loop
  about: Loop mode
  driver: loop
  args:
    - { id: target, long: target, required: true }
    - { id: while, long: while, conflicts_with: [until] }
    - { id: until, long: until }
    - { id: budget, long: budget, value: integer }
- kind: prompt
  driver: goal
  args:
    - { id: max_iterations, long: max-iterations, value: integer, default: "6" }
- name: workflow
  subcommands:
    - { name: list, api: list, args: [{ id: json, long: json, value: flag }] }
"#,
        );
        validate_cli("acme/demo", &commands, &BTreeSet::new(), true).unwrap();
    }

    #[test]
    fn rejects_malformed_contributions() {
        let cases = [
            ("- { name: Loop, driver: loop }", true),
            ("- { name: loop }", true),
            ("- { name: loop, driver: a, api: b }", true),
            ("- { name: loop, driver: loop }", false),
            ("- { name: loop, api: missing }", false),
            ("- { kind: prompt, name: x, driver: goal }", true),
            ("- { name: a, driver: d }\n- { name: a, driver: d }", true),
            (
                "- { name: a, driver: d, args: [{ id: x, long: x, conflicts_with: [y] }] }",
                true,
            ),
            (
                "- { name: a, driver: d, args: [{ id: f, long: f, value: flag, required: true }] }",
                true,
            ),
        ];
        for (yaml, native) in cases {
            assert!(
                validate_cli("acme/demo", &parse(yaml), &BTreeSet::new(), native).is_err(),
                "{yaml}"
            );
        }
    }
}
