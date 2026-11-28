//! `hya` commands contributed by enabled bundles (`cli:` in `bundle.yaml`,
//! ADR-0040): built into the command line at startup and routed to the
//! bundle's driver or BundleApi endpoint (on `--db` with `database: true`).

use std::ffi::OsString;

use anyhow::Context as _;
use clap::{
    Arg, ArgAction, ArgMatches, Command as ClapCommand, CommandFactory, FromArgMatches, Parser as _,
};
use hya_app::CliContribution;
use hya_bundle::{CliArg, CliCommand, CliCommandKind, CliGlobalFlag, CliRoutedRefusal, CliValue};
use serde_json::{Map, Value};

use crate::cli_args::Cli;

/// Where a parsed bundle command goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    /// The driver `<bundle-id>:<driver>`.
    Driver(String),
    /// The BundleApi endpoint `api` of `bundle`; on `--db` with `database`.
    Api {
        bundle: String,
        api: String,
        database: Option<Database>,
    },
}

/// A `database: true` api route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Database {
    /// Top-level command name, for messages (`hya <command>: …`).
    pub(crate) command: String,
    /// Id of the `value: session` argument.
    session_arg: Option<String>,
    refuse: Vec<CliRoutedRefusal>,
}

impl Database {
    /// The Session id the arguments name, as typed.
    pub(crate) fn session<'a>(&self, args: &'a Value) -> Option<&'a str> {
        self.session_arg
            .as_ref()
            .and_then(|id| args[id.as_str()].as_str())
    }

    /// The reason of the first `routed_refuse` entry that is set.
    pub(crate) fn refusal(&self, args: &Value, cli: &Cli) -> Option<&str> {
        self.refuse
            .iter()
            .find(|refusal| match (&refusal.arg, refusal.flag) {
                (Some(arg), _) => match &args[arg.as_str()] {
                    Value::Null | Value::Bool(false) => false,
                    Value::Array(list) => !list.is_empty(),
                    _ => true,
                },
                (None, Some(CliGlobalFlag::Bare)) => cli.bare,
                (None, Some(CliGlobalFlag::Pure)) => cli.pure,
                (None, Some(CliGlobalFlag::Yolo)) => cli.yolo,
                (None, None) => false,
            })
            .map(|refusal| refusal.reason.as_str())
    }
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
            parse_with(argv, &contributions).map_err(|error| {
                match error.downcast::<clap::Error>() {
                    Ok(clap) => clap.exit(),
                    Err(error) => error,
                }
            })
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
    let mut matches = command.try_get_matches_from(argv)?;
    let bundle_command = matches.subcommand_name().and_then(|name| {
        commands
            .iter()
            .find(|c| c.command.name.as_deref() == Some(name))
    });
    if let Some(contribution) = bundle_command
        && let Some((_, sub)) = matches.remove_subcommand()
    {
        let cli = Cli::from_arg_matches(&matches)?;
        let command = &contribution.command;
        let name = command.name.as_deref().unwrap_or_default();
        let (route, args) = resolve(&contribution.bundle, name, command, &sub)?;
        return Ok(Parsed::Bundle { cli, route, args });
    }
    finish_core(matches, prompt)
}

fn finish_core(matches: ArgMatches, prompt: Option<&CliContribution>) -> anyhow::Result<Parsed> {
    let cli = Cli::from_arg_matches(&matches)?;
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
                Some(route_of(&contribution.bundle, "-p", &contribution.command)?),
                args,
            )
        }
        None => (None, Value::Null),
    };
    Ok(Parsed::Prompt { cli, route, args })
}

/// Follow subcommands down to the leaf and collect its arguments; `top` is
/// the top-level command name.
fn resolve(
    bundle: &str,
    top: &str,
    command: &CliCommand,
    matches: &ArgMatches,
) -> anyhow::Result<(Route, Value)> {
    if let Some((name, sub)) = matches.subcommand()
        && let Some(child) = command
            .subcommands
            .iter()
            .find(|child| child.name.as_deref() == Some(name))
    {
        return resolve(bundle, top, child, sub);
    }
    Ok((
        route_of(bundle, top, command)?,
        collect_args(&command.args, matches),
    ))
}

fn route_of(bundle: &str, top: &str, command: &CliCommand) -> anyhow::Result<Route> {
    match (&command.driver, &command.api) {
        (Some(driver), None) => Ok(Route::Driver(format!("{bundle}:{driver}"))),
        (None, Some(api)) => Ok(Route::Api {
            bundle: bundle.to_string(),
            api: api.clone(),
            database: command.database.then(|| Database {
                command: top.to_string(),
                session_arg: command
                    .args
                    .iter()
                    .find(|arg| arg.value == CliValue::Session)
                    .map(|arg| arg.id.clone()),
                refuse: command.routed_refuse.clone(),
            }),
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
    clap = clap.value_name(
        arg.value_name
            .clone()
            .unwrap_or_else(|| arg.id.to_uppercase()),
    );
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
        CliValue::String | CliValue::Session => clap,
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
            CliValue::String | CliValue::Session => matches
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
                api: "list".into(),
                database: None,
            }
        );
        assert_eq!(args, serde_json::json!({ "name": "docs" }));
    }

    #[test]
    fn database_routes_carry_the_command_name_session_and_refusals() {
        let contributions = contributions(
            r#"
- name: flow
  subcommands:
    - name: run
      api: run
      database: true
      args:
        - { id: name }
        - { id: session, long: session, value: session }
        - { id: revision, long: revision }
        - { id: input, long: input, value: list }
      routed_refuse:
        - { arg: revision, reason: "revision" }
        - { arg: input, reason: "input" }
        - { flag: yolo, reason: "yolo" }
"#,
        );
        let parse = |line: &str| {
            let Parsed::Bundle { cli, route, args } =
                parse_with(argv(line), &contributions).unwrap()
            else {
                panic!("expected a bundle command");
            };
            let Route::Api {
                database: Some(database),
                ..
            } = route
            else {
                panic!("expected a database route");
            };
            (cli, database, args)
        };
        let (cli, database, args) = parse("hya flow run demo --session ses_1");
        assert_eq!(database.command, "flow");
        assert_eq!(database.session(&args), Some("ses_1"));
        assert_eq!(database.refusal(&args, &cli), None);
        for (line, reason) in [
            ("hya flow run --revision r --input a=b", "revision"),
            ("hya flow run --input a=b --yolo", "input"),
            ("hya --yolo flow run", "yolo"),
        ] {
            let (cli, database, args) = parse(line);
            assert_eq!(database.session(&args), None, "{line}");
            assert_eq!(database.refusal(&args, &cli), Some(reason), "{line}");
        }
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

    fn first_party(ids: &[&str]) -> Vec<CliContribution> {
        ids.iter()
            .flat_map(|id| {
                let catalog = hya_bundle::first_party_bundle(id).unwrap();
                catalog.bundles()[0]
                    .cli()
                    .iter()
                    .map(|command| CliContribution {
                        bundle: (*id).to_string(),
                        command: command.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// `hya -p` and `hya loop` keep their pre-bundle flag surface.
    #[test]
    fn goal_and_loop_flags_are_byte_compatible() {
        let contributions = first_party(&["hya/goal", "hya/loop"]);
        let Parsed::Prompt { route, args, .. } = parse_with(
            argv("hya -p ship --max-iterations 3 --evaluator-model deep/o3-mini"),
            &contributions,
        )
        .unwrap() else {
            panic!("expected goal mode");
        };
        assert_eq!(route, Some(Route::Driver("hya/goal:goal".into())));
        assert_eq!(
            args,
            serde_json::json!({ "prompt": "ship", "max_iterations": 3, "evaluator_model": "deep/o3-mini" })
        );
        let Parsed::Prompt { args, .. } = parse_with(argv("hya -p ship"), &contributions).unwrap()
        else {
            panic!("expected goal mode");
        };
        assert_eq!(args["max_iterations"], 6);

        let Parsed::Bundle { route, args, .. } = parse_with(
            argv("hya loop --target green --budget 5 --while true --evaluator-model deep/o3-mini"),
            &contributions,
        )
        .unwrap() else {
            panic!("expected loop mode");
        };
        assert_eq!(route, Route::Driver("hya/loop:loop".into()));
        assert_eq!(
            args,
            serde_json::json!({
                "target": "green", "budget": 5, "max_iterations": null, "while": "true",
                "until": null, "evaluator_model": "deep/o3-mini",
            })
        );
        let Parsed::Bundle { args, .. } = parse_with(
            argv("hya loop --target docs --until false --max-iterations 3"),
            &contributions,
        )
        .unwrap() else {
            panic!("expected loop mode");
        };
        assert_eq!(
            (&args["max_iterations"], &args["until"]),
            (&serde_json::json!(3), &serde_json::json!("false"))
        );

        for rejected in [
            "hya loop --target x --while a --until b",
            "hya loop --budget 2",
        ] {
            assert!(
                parse_with(argv(rejected), &contributions).is_err(),
                "{rejected}"
            );
        }
    }

    #[test]
    fn a_bundle_cannot_shadow_a_core_command() {
        let shadow = contributions("- { name: serve, driver: x }");
        assert!(parse_with(argv("hya serve"), &shadow).is_err());
    }
}
