use clap::{Parser, Subcommand};

use crate::agent_cmd::AgentCommand;
use crate::auth_cmd::{AuthCommand, OauthCommand};
use crate::bundle_cmd::BundleCommand;

#[derive(Parser)]
#[command(
    name = "hya",
    version,
    about = "hya — a multi-agent coding agent",
    long_about = "hya — a multi-agent coding agent.\n\n\
        Run bare `hya` in a terminal to start the TUI and the WebUI: an in-process \
        server, the terminal TUI, and the WebUI on http://127.0.0.1:3250 (`--port` \
        to change). Needs Bun. Subcommands select headless, server, and admin areas."
)]
pub(crate) struct Cli {
    /// Headless goal mode: iterate the agent until an independent evaluator
    /// reports the goal met, or the iteration cap trips.
    #[arg(short = 'p', long = "prompt", value_name = "GOAL")]
    pub(crate) prompt: Option<String>,
    /// Iteration cap for `-p` goal mode.
    #[arg(long, default_value_t = 6)]
    pub(crate) max_iterations: u32,
    /// Evaluator model for `-p` goal mode as `provider/model`. Overrides the
    /// config `goal.evaluator_model`; without either, the worker's current
    /// model judges (unless a plugin registers `goal.evaluate`).
    #[arg(long, value_name = "PROVIDER/MODEL")]
    pub(crate) evaluator_model: Option<String>,
    /// Port of the WebUI that bare `hya` serves on 127.0.0.1 next to the
    /// terminal TUI (default 3250; 0 picks a free port). Only for bare `hya`.
    #[arg(long, value_name = "PORT")]
    pub(crate) port: Option<u16>,
    /// Model id to use (overrides config `default_model` + `HYA_MODEL`).
    #[arg(long, global = true, value_name = "MODEL")]
    pub(crate) model: Option<String>,
    /// Auto-approve every tool action (edit/write/shell anywhere). Use with care.
    #[arg(long, global = true)]
    pub(crate) yolo: bool,
    #[arg(long = "print-logs", global = true)]
    pub(crate) print_logs: bool,
    #[arg(long = "log-level", global = true, value_parser = ["DEBUG", "INFO", "WARN", "ERROR"])]
    pub(crate) log_level: Option<String>,
    #[arg(long, global = true)]
    pub(crate) pure: bool,
    /// SQLite database path. Applies to headless exec/run persistence, serve,
    /// bare `hya`, sessions, and replay. Empty: an in-memory store for
    /// exec/run/serve; `$XDG_STATE_HOME/hya/sessions.db` for bare `hya`,
    /// sessions, and tail-session.
    #[arg(long, global = true, default_value = "")]
    pub(crate) db: String,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Compat-compatible alias for headless prompt execution.
    Run {
        /// Message words to send to the agent.
        message: Vec<String>,
        /// Format: default transcript or JSONL event stream.
        #[arg(long, value_parser = ["default", "json"], default_value = "default")]
        format: String,
        /// Emit the event stream as JSONL instead of a rendered transcript.
        #[arg(long)]
        json: bool,
    },
    /// Run a single prompt headlessly and print the resulting transcript.
    Exec {
        /// The user prompt to send to the agent.
        prompt: String,
        /// Emit the event stream as JSONL instead of a rendered transcript.
        #[arg(long)]
        json: bool,
    },
    /// Start the HTTP + SSE server.
    Serve {
        /// Address to bind. Use `127.0.0.1:0` for an ephemeral port.
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: String,
        /// Hostname to listen on. Compat-compatible alias for the host part of `--bind`.
        #[arg(long)]
        hostname: Option<String>,
        /// Port to listen on. Compat-compatible alias for the port part of `--bind`.
        #[arg(long)]
        port: Option<u16>,
        /// Enable Compat mDNS-compatible wildcard binding when hostname is not set.
        #[arg(long)]
        mdns: bool,
        /// Accepted for Compat CLI compatibility; hya does not advertise mDNS yet.
        #[arg(long = "mdns-domain", default_value = "compat.local")]
        mdns_domain: String,
        /// Accepted for Compat CLI compatibility; hya mirrors CORS origins globally.
        #[arg(long)]
        cors: Vec<String>,
        /// Override global SQLite database path for this server.
        #[arg(long)]
        db: Option<String>,
    },
    /// Replay a session's event log from a database as JSON lines.
    TailSession {
        /// Session id (`hysec_...`, `ses_...`, or legacy raw UUID).
        id: String,
        /// Override global SQLite database path for replay.
        #[arg(long)]
        db: Option<String>,
    },
    /// Save an auth token for a provider id (used instead of an inline api_key).
    Login {
        /// Provider id as it appears in your hya config.
        provider: String,
        /// The bearer/API token to store.
        token: String,
    },
    /// Interactive OAuth login / status for openai-codex and grok-build.
    Oauth {
        #[command(subcommand)]
        command: OauthCommand,
    },
    /// Inspect or remove saved provider auth tokens.
    #[command(alias = "providers")]
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Manage agents.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Manage agent bundles.
    Bundle {
        #[command(subcommand)]
        command: BundleCommand,
    },
    Workflow {
        #[command(subcommand)]
        command: crate::workflow_cmd::WorkflowCliCommand,
    },
    /// List the effective model catalog (model cache ∪ config entries).
    Models {
        /// Provider id to filter models by.
        provider: Option<String>,
        /// Include safe source/metadata per model.
        #[arg(long)]
        verbose: bool,
        /// Fetch every provider's remote model list (or only `provider`'s)
        /// into the model cache before listing.
        #[arg(long)]
        refresh: bool,
    },
    /// List sessions stored in a database (archived root sessions only with
    /// `--all` or `--archived`), or archive/unarchive one.
    Sessions {
        /// Override global SQLite database path (also after `archive` and
        /// `unarchive`).
        #[arg(long, global = true)]
        db: Option<String>,
        /// List archived root sessions too.
        #[arg(long, conflicts_with = "archived")]
        all: bool,
        /// List only archived root sessions.
        #[arg(long)]
        archived: bool,
        /// Archive or unarchive a session instead of listing.
        #[command(subcommand)]
        action: Option<crate::sessions_cmd::SessionsAction>,
    },
    /// JSONL RPC over stdin/stdout: read {"type":"prompt","text":...} lines, emit event JSONL.
    Rpc,
    /// Verify, stage, activate, or recover signed hya releases (update TCB).
    Update {
        #[command(subcommand)]
        command: hya_updater::cli::UpdateCommand,
    },
    /// Loop mode: iterate the agent toward `--target` until the deterministic
    /// `--while`/`--until` condition, the `loop.should_stop` hook, or the
    /// independent verifier stops the run.
    Loop {
        /// The loop target: what "done" means, judged independently.
        #[arg(long)]
        target: String,
        /// Iteration budget (clamped to 1..=100 by the engine's hard ceiling).
        #[arg(long)]
        budget: Option<u32>,
        /// Alias for `--budget` (mirrors `-p` goal mode's flag); `--budget`
        /// wins when both are given.
        #[arg(long)]
        max_iterations: Option<u32>,
        /// Keep looping while this shell command exits 0; exit 1 stops the
        /// loop; any other exit code or a timeout is a broken condition
        /// (a failure, never a success).
        #[arg(long = "while", value_name = "CMD")]
        while_command: Option<String>,
        /// Stop when this shell command exits 0; exit 1 keeps looping; any
        /// other exit code or a timeout is a broken condition. Mutually
        /// exclusive with `--while`.
        #[arg(long = "until", value_name = "CMD")]
        until_command: Option<String>,
        /// Verifier/planner model as `provider/model`. Overrides the config
        /// `goal.evaluator_model`; without either, the worker's current model
        /// judges.
        #[arg(long, value_name = "PROVIDER/MODEL")]
        evaluator_model: Option<String>,
    },
}

/// Resolve the effective loop budget from the parsed flags: `--budget`
/// outranks the `--max-iterations` alias, the default mirrors
/// [`hya_core::LoopConfig::default`], and every request is clamped into the
/// range `cost_preflight` enforces.
#[must_use]
pub(crate) fn loop_budget(budget: Option<u32>, max_iterations: Option<u32>) -> u32 {
    let default = hya_core::LoopConfig::default().budget;
    hya_core::loop_mode::clamp_budget(budget.or(max_iterations).unwrap_or(default))
}

/// Build the deterministic loop predicate from `--while`/`--until`: exactly
/// one may be set, evaluated in the session workdir.
pub(crate) fn build_loop_predicate(
    while_command: Option<String>,
    until_command: Option<String>,
    workdir: &std::path::Path,
) -> Result<Option<hya_core::loop_mode::LoopPredicate>, String> {
    match (while_command, until_command) {
        (Some(_), Some(_)) => Err(
            "--while and --until are mutually exclusive; give exactly one loop condition"
                .to_string(),
        ),
        (Some(command), None) => Ok(Some(hya_core::loop_mode::LoopPredicate::new(
            command,
            hya_core::loop_mode::PredicateMode::While,
            workdir.to_path_buf(),
        ))),
        (None, Some(command)) => Ok(Some(hya_core::loop_mode::LoopPredicate::new(
            command,
            hya_core::loop_mode::PredicateMode::Until,
            workdir.to_path_buf(),
        ))),
        (None, None) => Ok(None),
    }
}

/// Default WebUI port of bare `hya` (`--port` overrides it).
pub(crate) const DEFAULT_WEB_PORT: u16 = 3250;

/// The WebUI port of bare `hya`: `--port`, else [`DEFAULT_WEB_PORT`].
///
/// `--port` is a top-level flag of the bare invocation only; with a
/// subcommand or `-p` goal mode it is an error rather than silently ignored
/// (`serve --port` is the serve subcommand's own flag).
pub(crate) fn bare_web_port(cli: &Cli) -> anyhow::Result<u16> {
    if cli.port.is_some() && (cli.command.is_some() || cli.prompt.is_some()) {
        anyhow::bail!(
            "--port only applies to bare `hya` (the WebUI port); use `hya serve --port` for the server"
        );
    }
    Ok(cli.port.unwrap_or(DEFAULT_WEB_PORT))
}

pub(crate) fn serve_bind(
    bind: String,
    hostname: Option<String>,
    port: Option<u16>,
    mdns: bool,
) -> String {
    if hostname.is_none() && port.is_none() && !mdns {
        return bind;
    }
    let (default_host, default_port) = bind.rsplit_once(':').unwrap_or((&bind, "8080"));
    let host = hostname.unwrap_or_else(|| {
        let host = if mdns { "0.0.0.0" } else { default_host };
        host.to_string()
    });
    let port = port.map_or_else(|| default_port.to_string(), |port| port.to_string());
    format!("{host}:{port}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use clap::CommandFactory as _;
    use clap::Parser as _;

    use super::Cli;

    fn parse<const N: usize>(args: [&str; N]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|err| panic!("{err}"))
    }

    fn parse_slice(args: &[&str]) -> Cli {
        Cli::try_parse_from(args.iter().copied()).unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn executable_is_named_hya() {
        assert_eq!(Cli::command().get_name(), "hya");
    }

    #[test]
    fn parses_update_subcommands() {
        let cases: &[&[&str]] = &[
            &["hya", "update", "version"],
            &["hya", "update", "status", "--root", "/tmp/updater"],
            &["hya", "update", "recover", "--root", "/tmp/updater"],
            &[
                "hya",
                "update",
                "discard",
                "--root",
                "/tmp/updater",
                "--sequence",
                "4",
            ],
            &[
                "hya",
                "update",
                "apply",
                "--root",
                "/tmp/updater",
                "--metadata",
                "m.json",
                "--package",
                "pkg",
                "--platform",
                "x86_64-unknown-linux-gnu",
                "--owner-authorized-activation",
            ],
            &[
                "hya",
                "update",
                "init-roots",
                "--path",
                "t.json",
                "--root",
                "k=00",
            ],
        ];
        for args in cases {
            let cli = parse_slice(args);
            assert!(
                matches!(cli.command, Some(super::Command::Update { .. })),
                "{args:?} must parse as `hya update`"
            );
        }
    }

    #[test]
    fn parses_bundle_scope_confirmation_and_verify_flags() {
        use crate::bundle_cmd::BundleCommand;

        for verb in ["remove", "uninstall"] {
            let cli = parse_slice(&["hya", "bundle", verb, "-y", "--project", "hya/demo"]);
            assert!(
                matches!(
                    cli.command,
                    Some(super::Command::Bundle {
                        command: BundleCommand::Remove { .. }
                    })
                ),
                "`bundle {verb}` must parse as remove"
            );
        }
        for args in [
            &["hya", "bundle", "install", "-y", "demo.hyabundle"][..],
            &[
                "hya",
                "bundle",
                "install",
                "--yes",
                "--user",
                "demo.hyabundle",
            ],
            &["hya", "bundle", "install", "--project", "demo.hyabundle"],
            &["hya", "bundle", "verify", "demo.hyabundle"],
            &[
                "hya",
                "bundle",
                "verify",
                "--project",
                "--overwrite",
                "demo.hyabundle",
            ],
            &["hya", "bundle", "list", "--project"],
            &["hya", "bundle", "list", "--user"],
            &["hya", "bundle", "info", "--project", "hya/demo"],
            &["hya", "bundle", "info", "demo.hyabundle"],
            &["hya", "bundle", "search", "--project", "demo"],
            &["hya", "bundle", "schema", "hya/demo"],
            &["hya", "bundle", "schema", "--user", "hya/demo"],
            &["hya", "bundle", "schema", "demo.hyabundle"],
        ] {
            parse_slice(args);
        }
        let conflict = Cli::try_parse_from([
            "hya",
            "bundle",
            "install",
            "--user",
            "--project",
            "demo.hyabundle",
        ]);
        assert!(conflict.is_err(), "--user and --project are exclusive");
        let verify_yes = Cli::try_parse_from(["hya", "bundle", "verify", "-y", "demo.hyabundle"]);
        assert!(
            verify_yes.is_err(),
            "verify never installs, so it takes no -y"
        );
    }

    #[test]
    fn rejects_mini_as_unknown_argument() {
        let err = match Cli::try_parse_from(["hya", "--mini"]) {
            Ok(_) => panic!("--mini should be rejected once legacy TUI is removed"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
        assert!(err.to_string().contains("--mini"));
    }
    #[test]
    fn help_omits_mini_alias() {
        let help = Cli::command().render_help().to_string();

        assert!(!help.contains("--mini"));
    }

    #[test]
    fn parses_bundle_install_list_uninstall_and_info_file_commands() {
        let cases: &[&[&str]] = &[
            &["hya", "bundle", "install", "demo.hyabundle"],
            &["hya", "bundle", "list"],
            &["hya", "bundle", "uninstall", "hya/demo"],
            &["hya", "bundle", "info", "hya/demo"],
            &["hya", "bundle", "info", "-f", "demo.hyabundle"],
        ];

        for args in cases {
            let parsed = Cli::try_parse_from(args.iter().copied());
            let error = parsed.as_ref().err().map(ToString::to_string);
            assert!(parsed.is_ok(), "failed to parse {args:?}: {error:?}");
        }
    }

    #[test]
    fn rejects_resume_as_unknown_argument() {
        let err = match Cli::try_parse_from(["hya", "--resume", "hysec_abcdefghijklmnopqrst"]) {
            Ok(_) => panic!("--resume should be rejected once the interactive TUI is removed"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
        assert!(err.to_string().contains("--resume"));
    }

    #[test]
    fn parses_evaluator_model_flag_for_goal_mode() {
        let cli = parse(["hya", "-p", "ship it", "--evaluator-model", "deep/o3-mini"]);
        assert_eq!(cli.evaluator_model.as_deref(), Some("deep/o3-mini"));
        let cli = parse(["hya", "-p", "ship it"]);
        assert!(
            cli.evaluator_model.is_none(),
            "the flag must stay optional so config/worker fallback applies"
        );
    }

    #[test]
    fn parses_loop_command_flags() {
        let cli = parse([
            "hya",
            "loop",
            "--target",
            "tests are green",
            "--budget",
            "5",
            "--while",
            "cargo test --quiet",
            "--evaluator-model",
            "deep/o3-mini",
        ]);
        match cli.command {
            Some(super::Command::Loop {
                target,
                budget,
                max_iterations,
                while_command,
                until_command,
                evaluator_model,
            }) => {
                assert_eq!(target, "tests are green");
                assert_eq!(budget, Some(5));
                assert_eq!(max_iterations, None);
                assert_eq!(while_command.as_deref(), Some("cargo test --quiet"));
                assert_eq!(until_command, None);
                assert_eq!(evaluator_model.as_deref(), Some("deep/o3-mini"));
            }
            _ => panic!("expected loop command"),
        }
    }

    #[test]
    fn parses_loop_until_and_max_iterations_alias() {
        let cli = parse([
            "hya",
            "loop",
            "--target",
            "docs rebuilt",
            "--until",
            "test -f done",
            "--max-iterations",
            "3",
        ]);
        match cli.command {
            Some(super::Command::Loop {
                budget,
                max_iterations,
                while_command,
                until_command,
                ..
            }) => {
                assert_eq!(budget, None);
                assert_eq!(max_iterations, Some(3));
                assert_eq!(while_command, None);
                assert_eq!(until_command.as_deref(), Some("test -f done"));
            }
            _ => panic!("expected loop command"),
        }
    }

    #[test]
    fn loop_command_requires_target_and_defaults_flags() {
        let cli = parse(["hya", "loop", "--target", "ship it"]);
        match cli.command {
            Some(super::Command::Loop {
                target,
                budget,
                max_iterations,
                while_command,
                until_command,
                evaluator_model,
            }) => {
                assert_eq!(target, "ship it");
                assert_eq!(budget, None);
                assert_eq!(max_iterations, None);
                assert_eq!(while_command, None);
                assert_eq!(until_command, None);
                assert_eq!(evaluator_model, None);
            }
            _ => panic!("expected loop command"),
        }
    }

    #[test]
    fn loop_budget_resolves_alias_default_and_clamps() {
        use hya_core::loop_mode::{LoopConfig, cost_preflight};
        // Default mirrors `LoopConfig::default().budget`.
        assert_eq!(super::loop_budget(None, None), LoopConfig::default().budget);
        // `--budget` outranks the `--max-iterations` alias.
        assert_eq!(super::loop_budget(Some(5), Some(9)), 5);
        assert_eq!(super::loop_budget(None, Some(9)), 9);
        // Any request is clamped into the range `cost_preflight` enforces
        // (HARD_MAX_ITERATIONS / minimum 1), so the resolved value preflights.
        let clamped = super::loop_budget(None, Some(500));
        assert!(
            cost_preflight(&LoopConfig {
                budget: clamped,
                ..LoopConfig::default()
            })
            .is_ok(),
            "clamped budget {clamped} must pass cost_preflight"
        );
        assert_eq!(super::loop_budget(Some(0), None), 1);
    }

    #[test]
    fn build_loop_predicate_maps_flags_to_modes() {
        let workdir = std::env::temp_dir();
        let while_only =
            super::build_loop_predicate(Some("test -f x".to_string()), None, &workdir).unwrap();
        assert!(while_only.is_some(), "--while builds a predicate");
        let predicate = while_only.unwrap();
        assert_eq!(predicate.mode, hya_core::loop_mode::PredicateMode::While);
        assert_eq!(predicate.command, "test -f x");
        assert_eq!(predicate.workdir, workdir);

        let until_only =
            super::build_loop_predicate(None, Some("false".to_string()), &workdir).unwrap();
        assert_eq!(
            until_only.unwrap().mode,
            hya_core::loop_mode::PredicateMode::Until
        );

        assert!(
            super::build_loop_predicate(None, None, &workdir)
                .unwrap()
                .is_none(),
            "no flags means no predicate (verifier decides)"
        );

        let both = super::build_loop_predicate(
            Some("true".to_string()),
            Some("false".to_string()),
            &workdir,
        );
        assert!(both.is_err(), "--while and --until are mutually exclusive");
    }

    #[test]
    fn parses_compat_run_alias() {
        let cli = parse(["hya", "run", "--format", "json", "hello", "world"]);
        match cli.command {
            Some(super::Command::Run {
                message,
                json,
                format,
            }) => {
                assert!(!json);
                assert_eq!(format, "json");
                assert_eq!(message, ["hello", "world"]);
            }
            _ => panic!("expected run command"),
        }
    }

    #[test]
    fn bare_port_defaults_to_3250_and_accepts_overrides() {
        assert_eq!(super::bare_web_port(&parse(["hya"])).unwrap(), 3250);
        assert_eq!(
            super::bare_web_port(&parse(["hya", "--port", "8000"])).unwrap(),
            8000
        );
        assert_eq!(
            super::bare_web_port(&parse(["hya", "--port", "0"])).unwrap(),
            0
        );
        assert!(Cli::try_parse_from(["hya", "--port", "70000"]).is_err());
        assert!(Cli::try_parse_from(["hya", "--port", "web"]).is_err());
    }

    #[test]
    fn bare_port_is_rejected_with_a_subcommand_or_goal_mode() {
        let error = super::bare_web_port(&parse(["hya", "--port", "9000", "sessions"]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("--port only applies to bare `hya`"),
            "{error}"
        );
        let error = super::bare_web_port(&parse(["hya", "--port", "9000", "-p", "goal"]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("--port only applies to bare `hya`"),
            "{error}"
        );
        // `serve --port` stays the serve subcommand's own flag.
        let cli = parse(["hya", "serve", "--port", "4096"]);
        assert!(cli.port.is_none());
        assert_eq!(super::bare_web_port(&cli).unwrap(), 3250);
    }

    #[test]
    fn help_documents_the_bare_port_flag() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("--port <PORT>"), "{help}");
        assert!(help.contains("3250"), "{help}");
        assert!(help.contains("WebUI"), "{help}");
    }

    #[test]
    fn parses_compat_serve_network_aliases() {
        let cli = parse(["hya", "serve", "--hostname", "0.0.0.0", "--port", "4096"]);
        match cli.command {
            Some(super::Command::Serve {
                bind,
                hostname,
                port,
                ..
            }) => {
                assert_eq!(bind, "127.0.0.1:8080");
                assert_eq!(hostname.as_deref(), Some("0.0.0.0"));
                assert_eq!(port, Some(4096));
                assert_eq!(
                    super::serve_bind(bind, hostname, port, false),
                    "0.0.0.0:4096"
                );
            }
            _ => panic!("expected serve command"),
        }
    }

    #[test]
    fn parses_compat_serve_cors_and_mdns_flags() {
        let cli = parse([
            "hya",
            "serve",
            "--mdns",
            "--mdns-domain",
            "hya.local",
            "--cors",
            "https://app.test",
        ]);
        match cli.command {
            Some(super::Command::Serve {
                bind,
                hostname,
                port,
                mdns,
                mdns_domain,
                cors,
                ..
            }) => {
                assert!(mdns);
                assert_eq!(mdns_domain, "hya.local");
                assert_eq!(cors, ["https://app.test"]);
                assert_eq!(
                    super::serve_bind(bind, hostname, port, mdns),
                    "0.0.0.0:8080"
                );
            }
            _ => panic!("expected serve command"),
        }
    }

    #[test]
    fn parses_models_command_without_refresh() {
        let cli = parse(["hya", "models", "openai", "--verbose"]);
        match cli.command {
            Some(super::Command::Models {
                provider,
                verbose,
                refresh,
            }) => {
                assert_eq!(provider.as_deref(), Some("openai"));
                assert!(verbose);
                assert!(!refresh);
            }
            _ => panic!("expected models command"),
        }
    }

    #[test]
    fn parses_models_refresh_flag() {
        let cli = parse(["hya", "models", "--refresh"]);
        match cli.command {
            Some(super::Command::Models {
                provider, refresh, ..
            }) => {
                assert_eq!(provider, None);
                assert!(refresh);
            }
            _ => panic!("expected models command"),
        }
    }

    #[test]
    fn parses_compat_providers_alias_for_auth_list() {
        let cli = parse(["hya", "providers", "list"]);
        match cli.command {
            Some(super::Command::Auth {
                command: crate::auth_cmd::AuthCommand::List,
            }) => {}
            _ => panic!("expected auth list command"),
        }
    }

    #[test]
    fn parses_oauth_login_command() {
        let cli = parse([
            "hya",
            "oauth",
            "login",
            "--provider",
            "codex",
            "--type",
            "openai-codex",
            "--model",
            "gpt-5.3-codex",
        ]);
        match cli.command {
            Some(super::Command::Oauth {
                command:
                    crate::auth_cmd::OauthCommand::Login {
                        provider,
                        oauth_type,
                        device,
                        loopback,
                        no_browser,
                        browser,
                        model,
                        base_url,
                    },
            }) => {
                assert_eq!(provider, "codex");
                assert_eq!(oauth_type, "openai-codex");
                // Defaults applied at runtime: device + no-browser for openai-codex.
                assert!(!device);
                assert!(!loopback);
                assert!(!no_browser);
                assert!(!browser);
                assert_eq!(model.as_deref(), Some("gpt-5.3-codex"));
                assert!(base_url.is_none());
            }
            _ => panic!("expected oauth login command"),
        }
    }

    #[test]
    fn parses_oauth_login_loopback_and_browser_flags() {
        let cli = parse([
            "hya",
            "oauth",
            "login",
            "--provider",
            "codex",
            "--type",
            "openai-codex",
            "--loopback",
            "--browser",
        ]);
        match cli.command {
            Some(super::Command::Oauth {
                command:
                    crate::auth_cmd::OauthCommand::Login {
                        loopback,
                        browser,
                        no_browser,
                        ..
                    },
            }) => {
                assert!(loopback);
                assert!(browser);
                assert!(!no_browser);
            }
            _ => panic!("expected oauth login with loopback/browser"),
        }
    }

    #[test]
    fn parses_bundle_search_query_and_requires_one() {
        let cli = parse(["hya", "bundle", "search", "goal-loop"]);
        match cli.command {
            Some(super::Command::Bundle {
                command: super::BundleCommand::Search { query, .. },
            }) => {
                assert_eq!(query, "goal-loop");
            }
            _ => panic!("expected bundle search command"),
        }
        let error = Cli::try_parse_from(["hya", "bundle", "search"])
            .err()
            .expect("bundle search without a query must fail to parse");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }
}
