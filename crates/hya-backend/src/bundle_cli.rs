//! `hya` commands contributed by enabled bundles (`cli:` in `bundle.yaml`,
//! ADR-0040): built into the command line at startup and routed to the
//! bundle's driver or BundleApi endpoint.

use std::ffi::OsString;

use anyhow::Context as _;
use clap::{
    Arg, ArgAction, ArgMatches, Command as ClapCommand, CommandFactory, FromArgMatches, Parser as _,
};
use hya_app::CliContribution;
use hya_bundle::{CliArg, CliCommand, CliCommandKind, CliValue};
use serde_json::{Map, Value};

use crate::cli_args::Cli;

/// Where a parsed bundle command goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    /// The driver `<bundle-id>:<driver>`.
    Driver(String),
    /// The BundleApi endpoint `api` of `bundle`.
    Api { bundle: String, api: String },
}

impl Route {
    /// Short name used in error prefixes (`goal error: …`).
    pub(crate) fn label(&self) -> &str {
        match self {
            Self::Driver(name) => name
                .rsplit_once(':')
                .map_or(name.as_str(), |(_, local)| local),
            Self::Api { api, .. } => api,
        }
    }
}

/// A parsed command line.
pub(crate) enum Parsed {
    /// A core command (or bare `hya`).
    Core(Cli),
    /// `hya -p <PROMPT>`: through the prompt driver when an enabled bundle
    /// claims `-p`, else one headless turn.
    Prompt {
        cli: Cli,
        route: Option<Route>,
        args: Value,
    },
    /// A bundle-contributed command.
    Bundle { cli: Cli, route: Route, args: Value },
}

/// Parse `argv`. The core command line is tried first; bundle contributions
/// are loaded only when it does not parse, when help is asked, or for `-p`.
pub(crate) async fn parse(argv: Vec<OsString>) -> anyhow::Result<Parsed> {
    match Cli::try_parse_from(&argv) {
        Ok(cli) if cli.prompt.is_none() => Ok(Parsed::Core(cli)),
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayVersion => error.exit(),
        _ => {
            let bare = argv.iter().skip(1).any(|arg| arg == "--bare");
            let contributions = hya_app::cli_contributions(bare)
                .await
                .context("load bundle CLI contributions")?;
            parse_with(argv, &contributions)
        }
    }
}

fn parse_with(argv: Vec<OsString>, contributions: &[CliContribution]) -> anyhow::Result<Parsed> {
    let mut command = Cli::command();
    let core_names: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_string())
        .collect();
    let mut prompt: Option<&CliContribution> = None;
    let mut commands: Vec<&CliContribution> = Vec::new();
    for contribution in contributions {
        match contribution.command.kind {
            CliCommandKind::Prompt => {
                if let Some(first) = prompt {
                    anyhow::bail!(
                        "bundles `{}` and `{}` both claim `hya -p`; disable one of them",
                        first.bundle,
                        contribution.bundle
                    );
                }
                prompt = Some(contribution);
                for arg in &contribution.command.args {
                    command = command.arg(clap_arg(arg));
                }
            }
            CliCommandKind::Command => {
                let name = contribution.command.name.as_deref().unwrap_or_default();
                if core_names.iter().any(|core| core == name) {
                    anyhow::bail!(
                        "bundle `{}` contributes `hya {name}`, which is a core command",
                        contribution.bundle
                    );
                }
                if let Some(other) = commands
                    .iter()
                    .find(|other| other.command.name == contribution.command.name)
                {
                    anyhow::bail!(
                        "bundles `{}` and `{}` both contribute `hya {name}`; disable one of them",
                        other.bundle,
                        contribution.bundle
                    );
                }
                commands.push(contribution);
                command = command.subcommand(clap_command(&contribution.command));
            }
        }
    }
    let mut matches = command
        .try_get_matches_from(argv)
        .unwrap_or_else(|error| error.exit());
    let bundle_command = matches.subcommand_name().and_then(|name| {
        commands
            .iter()
            .find(|c| c.command.name.as_deref() == Some(name))
    });
    if let Some(contribution) = bundle_command
        && let Some((_, sub)) = matches.remove_subcommand()
    {
        let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
        let (route, args) = resolve(&contribution.bundle, &contribution.command, &sub)?;
        return Ok(Parsed::Bundle { cli, route, args });
    }
    finish_core(matches, prompt)
}

fn finish_core(matches: ArgMatches, prompt: Option<&CliContribution>) -> anyhow::Result<Parsed> {
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    if cli.prompt.is_none() {
        return Ok(Parsed::Core(cli));
    }
    let (route, args) = match prompt {
        Some(contribution) => {
            let mut args = collect_args(&contribution.command.args, &matches);
            if let Value::Object(map) = &mut args {
                map.insert(
                    "prompt".into(),
                    Value::String(cli.prompt.clone().unwrap_or_default()),
                );
            }
            (
                Some(route_of(&contribution.bundle, &contribution.command)?),
                args,
            )
        }
        None => (None, Value::Null),
    };
    Ok(Parsed::Prompt { cli, route, args })
}

/// Follow subcommands down to the leaf and collect its arguments.
fn resolve(
    bundle: &str,
    command: &CliCommand,
    matches: &ArgMatches,
) -> anyhow::Result<(Route, Value)> {
    if let Some((name, sub)) = matches.subcommand()
        && let Some(child) = command
            .subcommands
            .iter()
            .find(|child| child.name.as_deref() == Some(name))
    {
        return resolve(bundle, child, sub);
    }
    Ok((
        route_of(bundle, command)?,
        collect_args(&command.args, matches),
    ))
}

fn route_of(bundle: &str, command: &CliCommand) -> anyhow::Result<Route> {
    match (&command.driver, &command.api) {
        (Some(driver), None) => Ok(Route::Driver(format!("{bundle}:{driver}"))),
        (None, Some(api)) => Ok(Route::Api {
            bundle: bundle.to_string(),
            api: api.clone(),
        }),
        _ => anyhow::bail!("bundle `{bundle}` declares a cli command without one route"),
    }
}

fn clap_command(command: &CliCommand) -> ClapCommand {
    let mut clap =
        ClapCommand::new(command.name.clone().unwrap_or_default()).about(command.about.clone());
    for arg in &command.args {
        clap = clap.arg(clap_arg(arg));
    }
    for sub in &command.subcommands {
        clap = clap.subcommand(clap_command(sub));
    }
    if !command.subcommands.is_empty() {
        clap = clap.subcommand_required(true).arg_required_else_help(true);
    }
    clap
}

fn clap_arg(arg: &CliArg) -> Arg {
    let mut clap = Arg::new(arg.id.clone()).help(arg.help.clone());
    if let Some(long) = &arg.long {
        clap = clap.long(long.clone());
    }
    if let Some(short) = arg.short {
        clap = clap.short(short);
    }
    if !arg.aliases.is_empty() {
        clap = clap.aliases(arg.aliases.clone());
    }
    if let Some(value_name) = &arg.value_name {
        clap = clap.value_name(value_name.clone());
    }
    if let Some(default) = &arg.default {
        clap = clap.default_value(default.clone());
    }
    if !arg.conflicts_with.is_empty() {
        clap = clap.conflicts_with_all(arg.conflicts_with.clone());
    }
    clap = match arg.value {
        CliValue::Flag => clap.action(ArgAction::SetTrue),
        CliValue::List => clap.action(ArgAction::Append),
        CliValue::Integer => clap.value_parser(clap::value_parser!(u64)),
        CliValue::String => clap,
    };
    clap.required(arg.required)
}

fn collect_args(args: &[CliArg], matches: &ArgMatches) -> Value {
    let mut out = Map::new();
    for arg in args {
        let value = match arg.value {
            CliValue::Flag => Value::Bool(matches.get_flag(&arg.id)),
            CliValue::Integer => matches
                .get_one::<u64>(&arg.id)
                .map_or(Value::Null, |value| Value::from(*value)),
            CliValue::String => matches
                .get_one::<String>(&arg.id)
                .map_or(Value::Null, |value| Value::String(value.clone())),
            CliValue::List => Value::Array(
                matches
                    .get_many::<String>(&arg.id)
                    .into_iter()
                    .flatten()
                    .map(|value| Value::String(value.clone()))
                    .collect(),
            ),
        };
        out.insert(arg.id.clone(), value);
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn contributions(yaml: &str) -> Vec<CliContribution> {
        let commands: Vec<CliCommand> = serde_norway::from_str(yaml).unwrap();
        commands
            .into_iter()
            .map(|command| CliContribution {
                bundle: "acme/demo".into(),
                command,
            })
            .collect()
    }

    fn argv(line: &str) -> Vec<OsString> {
        line.split_whitespace().map(OsString::from).collect()
    }

    const DEMO: &str = r#"
- name: echo
  driver: echo
  args:
    - { id: text, long: text, required: true }
    - { id: times, long: times, value: integer, default: "1" }
    - { id: loud, long: loud, value: flag }
    - { id: tag, long: tag, value: list }
- kind: prompt
  driver: goal
  args:
    - { id: rounds, long: rounds, value: integer, default: "6" }
- name: group
  subcommands:
    - { name: list, api: list, args: [{ id: name }] }
"#;

    #[test]
    fn bundle_commands_parse_into_their_route_and_arguments() {
        let parsed = parse_with(
            argv("hya echo --text hi --tag a --tag b --loud"),
            &contributions(DEMO),
        )
        .unwrap();
        let Parsed::Bundle { route, args, .. } = parsed else {
            panic!("expected a bundle command");
        };
        assert_eq!(route, Route::Driver("acme/demo:echo".into()));
        assert_eq!(
            args,
            serde_json::json!({ "text": "hi", "times": 1, "loud": true, "tag": ["a", "b"] })
        );

        let Parsed::Bundle { route, args, .. } =
            parse_with(argv("hya group list docs"), &contributions(DEMO)).unwrap()
        else {
            panic!("expected a nested bundle command");
        };
        assert_eq!(
            route,
            Route::Api {
                bundle: "acme/demo".into(),
                api: "list".into()
            }
        );
        assert_eq!(args, serde_json::json!({ "name": "docs" }));
    }

    #[test]
    fn prompt_claim_adds_top_level_flags_and_core_commands_still_parse() {
        let Parsed::Prompt { route, args, .. } =
            parse_with(argv("hya -p ship --rounds 3"), &contributions(DEMO)).unwrap()
        else {
            panic!("expected a prompt run");
        };
        assert_eq!(route, Some(Route::Driver("acme/demo:goal".into())));
        assert_eq!(args, serde_json::json!({ "prompt": "ship", "rounds": 3 }));

        let Parsed::Prompt { route, .. } = parse_with(argv("hya -p ship"), &[]).unwrap() else {
            panic!("expected a prompt run");
        };
        assert_eq!(route, None);

        assert!(matches!(
            parse_with(argv("hya models"), &contributions(DEMO)).unwrap(),
            Parsed::Core(_)
        ));
    }

    #[test]
    fn a_bundle_cannot_shadow_a_core_command() {
        let shadow = contributions("- { name: serve, driver: x }");
        assert!(parse_with(argv("hya serve"), &shadow).is_err());
    }
}
