//! Bundle-contributed `hya` CLI commands (`cli:` in `bundle.yaml`, ADR-0040).
//!
//! A bundle declares commands with their arguments; the `hya` binary adds
//! them to its command line when the bundle is enabled and routes each
//! invocation to the bundle's driver or BundleApi endpoint. An api route with
//! `database: true` runs against `--db`, through the server that owns it when
//! one does; one with `database: read` only reads `--db`, without claiming it.

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
    /// How the `api` route uses `--db` (default: not at all, an in-memory
    /// store).
    #[serde(default, skip_serializing_if = "CliDatabase::is_none")]
    pub database: CliDatabase,
    /// `database` routes only: refusals when the call would go through the
    /// owning server, checked in order; the first set one exits 75.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routed_refuse: Vec<CliRoutedRefusal>,
    /// Nested subcommands; a command with subcommands has no route itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcommands: Vec<CliCommand>,
}

/// [`CliCommand::database`]: `false` (default), `true`, or `read`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(from = "DatabaseRepr", into = "DatabaseRepr")]
pub enum CliDatabase {
    /// `false`: a headless in-memory store.
    #[default]
    None,
    /// `true`: claim `--db`, or go through the server that owns it.
    Write,
    /// `read`: never claim or route; resolve the working directory's Project
    /// from an existing `--db` opened read-only, and run on an in-memory
    /// store with the offline provider.
    Read,
}

impl CliDatabase {
    fn is_none(&self) -> bool {
        *self == Self::None
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(untagged)]
enum DatabaseRepr {
    Flag(bool),
    Mode(DatabaseMode),
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum DatabaseMode {
    Read,
}

impl From<DatabaseRepr> for CliDatabase {
    fn from(repr: DatabaseRepr) -> Self {
        match repr {
            DatabaseRepr::Flag(false) => Self::None,
            DatabaseRepr::Flag(true) => Self::Write,
            DatabaseRepr::Mode(DatabaseMode::Read) => Self::Read,
        }
    }
}

impl From<CliDatabase> for DatabaseRepr {
    fn from(database: CliDatabase) -> Self {
        match database {
            CliDatabase::None => Self::Flag(false),
            CliDatabase::Write => Self::Flag(true),
            CliDatabase::Read => Self::Mode(DatabaseMode::Read),
        }
    }
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
    /// An existing Session id (`database` routes only): a Session-scope
    /// endpoint is called for it instead of a new Session.
    Session,
}

impl CliValue {
    fn is_string(&self) -> bool {
        *self == Self::String
    }
}

/// One [`CliCommand::routed_refuse`] entry: exactly one of `arg` or `flag`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CliRoutedRefusal {
    /// Refuse when this argument of the command is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg: Option<String>,
    /// Refuse when this global flag is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<CliGlobalFlag>,
    /// Why the server cannot take the call, completing "…, and <reason>".
    pub reason: String,
}

/// A global `hya` flag a [`CliRoutedRefusal`] can name.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CliGlobalFlag {
    /// `--bare`.
    Bare,
    /// `--pure`.
    Pure,
    /// `--yolo`.
    Yolo,
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
    let sessions = command
        .args
        .iter()
        .filter(|arg| arg.value == CliValue::Session)
        .count();
    if command.database != CliDatabase::Write && (sessions > 0 || !command.routed_refuse.is_empty())
    {
        return Err(invalid(format!(
            "cli command `{label}`: `value: session` and `routed_refuse` need `database: true`"
        )));
    }
    if command.database != CliDatabase::None
        && (command.api.is_none() || command.kind == CliCommandKind::Prompt || sessions > 1)
    {
        return Err(invalid(format!(
            "cli command `{label}`: `database` needs an `api` route on `kind: command` and at most one `value: session` argument"
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
    for refusal in &command.routed_refuse {
        let known = match (&refusal.arg, refusal.flag) {
            (Some(arg), None) => ids.contains(arg.as_str()),
            (None, Some(_)) => true,
            _ => false,
        };
        if !known || refusal.reason.is_empty() {
            return Err(invalid(format!(
                "cli command `{label}`: each `routed_refuse` entry names one known `arg` or one `flag`, with a `reason`"
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
    - { name: list, api: list, database: read, args: [{ id: json, long: json, value: flag }] }
    - name: run
      api: run
      database: true
      args:
        - { id: session, long: session, value: session }
        - { id: revision, long: revision }
      routed_refuse:
        - { arg: revision, reason: "`--revision` cannot be sent to it" }
        - { flag: yolo, reason: "--yolo cannot apply to it" }
"#,
        );
        validate_cli("acme/demo", &commands, &BTreeSet::new(), true).unwrap();
        let workflow = &commands[2].subcommands;
        assert_eq!(
            [
                workflow[0].database,
                workflow[1].database,
                commands[0].database
            ],
            [CliDatabase::Read, CliDatabase::Write, CliDatabase::None]
        );
        let round_trip: Vec<CliCommand> =
            serde_norway::from_str(&serde_norway::to_string(&commands).unwrap()).unwrap();
        assert_eq!(round_trip, commands);
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
            ("- { name: a, driver: d, database: true }", true),
            ("- { kind: prompt, api: a, database: true }", true),
            ("- { name: a, driver: d, database: read }", true),
            (
                "- { name: a, api: a, database: read, args: [{ id: s, long: s, value: session }] }",
                true,
            ),
            (
                "- { name: a, api: a, database: read, routed_refuse: [{ flag: yolo, reason: r }] }",
                true,
            ),
            (
                "- { name: a, api: a, args: [{ id: s, long: s, value: session }] }",
                true,
            ),
            (
                "- { name: a, api: a, database: true, args: [{ id: s, long: s, value: session }, { id: t, long: t, value: session }] }",
                true,
            ),
            (
                "- { name: a, api: a, routed_refuse: [{ flag: yolo, reason: r }] }",
                true,
            ),
            (
                "- { name: a, api: a, database: true, routed_refuse: [{ arg: missing, reason: r }] }",
                true,
            ),
            (
                "- { name: a, api: a, database: true, args: [{ id: x, long: x }], routed_refuse: [{ arg: x, flag: yolo, reason: r }] }",
                true,
            ),
            (
                "- { name: a, api: a, database: true, routed_refuse: [{ flag: yolo, reason: \"\" }] }",
                true,
            ),
        ];
        assert!(
            serde_norway::from_str::<Vec<CliCommand>>("- { name: a, api: a, database: write }")
                .is_err()
        );
        for (yaml, native) in cases {
            assert!(
                validate_cli("acme/demo", &parse(yaml), &BTreeSet::new(), native).is_err(),
                "{yaml}"
            );
        }
    }
}
