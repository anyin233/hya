use clap::{Parser, Subcommand};

use crate::agent_cmd::AgentCommand;
use crate::auth_cmd::{AuthCommand, OauthCommand};
use crate::bundle_cmd::BundleCommand;
use crate::proxy_cmd::ProxyArgs;
use crate::relay_doctor::RelayCommand;

#[derive(Parser)]
#[command(
    name = "hya",
    version,
    about = "hya — a multi-agent coding agent",
    long_about = "hya — a multi-agent coding agent.\n\n\
        Run bare `hya` in a terminal to start the TUI and the WebUI: it connects to \
        the backend daemon of the database (starting one if none runs), then runs \
        the terminal TUI and the WebUI on http://127.0.0.1:3250 (`--port` to change). \
        The daemon keeps running after you quit; `hya serve status|stop|restart` \
        control it. Needs Bun. Subcommands select headless, server, and admin areas."
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
    /// Connect bare `hya` to this running backend (`http://host:port`)
    /// instead of the database's daemon: no discovery, no auto-start; an
    /// unreachable URL is an error. Only for bare `hya`.
    #[arg(long, value_name = "URL")]
    pub(crate) backend: Option<String>,
    /// Connect bare `hya` to a remote backend through a secure relay link:
    /// no local daemon; an in-process bridge carries the TUI and the WebUI
    /// to the backend, end-to-end encrypted. `--connect -` reads the link
    /// from the terminal without echoing it (recommended: an argument is
    /// visible in process listings); `--connect` alone reads
    /// `$HYA_RELAY_LINK`. Only for bare `hya`.
    #[arg(
        long,
        value_name = "LINK",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "backend"
    )]
    pub(crate) connect: Option<String>,
    /// PEM file of extra trusted CA certificates for the relay's TLS, used
    /// by `--connect`'s in-process bridge (a relay behind a private CA).
    /// Only with `--connect`.
    #[arg(long = "relay-ca", value_name = "PEM", requires = "connect")]
    pub(crate) connect_relay_ca: Option<std::path::PathBuf>,
    /// Relay binding of `--connect`'s in-process bridge: `auto`, `grpc`, or
    /// `ws`, overriding the link's `t=`. Only with `--connect`.
    #[arg(
        long = "transport",
        value_name = "BINDING",
        value_parser = ["auto", "grpc", "ws"],
        requires = "connect"
    )]
    pub(crate) connect_transport: Option<String>,
    /// Open this session in the terminal TUI and unarchive it; without an
    /// id, pick one of the directory's sessions (archived ones included).
    /// Only for bare `hya`.
    #[arg(long, value_name = "ID", num_args = 0..=1, default_missing_value = "")]
    pub(crate) resume: Option<String>,
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

/// `hya serve start|status|stop|restart`: the backend daemon of a database
/// (ADR-0023; docs/cli.md "Backend daemon").
#[derive(Subcommand, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ServeAction {
    /// Start the backend daemon for `--db` unless one is running, then print
    /// its URL and pid. The daemon runs detached (its own session, output to
    /// `<db>.server.log`) and outlives this command and every client.
    Start {
        /// Print `{url, pid, version, startedAt, db, log, started}` as JSON.
        #[arg(long)]
        json: bool,
        /// Join a secure relay at start (the daemon runs `hya serve --relay`).
        #[command(flatten)]
        relay: RelayFlags,
    },
    /// Show the running backend of `--db` (url, pid, version, db, uptime);
    /// exit 1 when none is running.
    Status {
        /// Print the status as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop the backend of `--db` gracefully (SIGTERM, then wait until it
    /// released the database). Stopping nothing is not an error.
    Stop {
        /// Kill it (SIGKILL) when it has not stopped within `--timeout`.
        #[arg(long)]
        force: bool,
        /// Seconds to wait for a graceful stop.
        #[arg(long, default_value_t = 30, value_name = "SECONDS")]
        timeout: u64,
    },
    /// Stop the backend of `--db` (if one runs), then start a new daemon.
    Restart {
        /// Print the new daemon as JSON (like `start --json`).
        #[arg(long)]
        json: bool,
        /// Kill the old backend when it has not stopped within `--timeout`.
        #[arg(long)]
        force: bool,
        /// Seconds to wait for the old backend to stop.
        #[arg(long, default_value_t = 30, value_name = "SECONDS")]
        timeout: u64,
        /// Join this relay instead of the one the old backend was joined to
        /// (by default the new daemon rejoins the old backend's relay).
        #[command(flatten)]
        relay: RelayFlags,
    },
    /// Control the secure relay of the running backend of `--db`
    /// (docs/relay.md "Hosting a backend on a relay"). Loopback only.
    Relay {
        #[command(subcommand)]
        action: ServeRelayAction,
    },
}

/// Relay flags of `hya serve`, `hya serve start`, and `hya serve restart`
/// (ADR-0025; docs/relay.md "Hosting a backend on a relay").
#[derive(clap::Args, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RelayFlags {
    /// Join the secure relay published at this public URL
    /// (`https://host[:port][/prefix]`, or `http://…` for plaintext on a
    /// LAN or tailnet) and print the relay link once on stderr. The link is
    /// a secret: whoever holds it controls this backend.
    #[arg(long = "relay", value_name = "URL")]
    pub(crate) relay: Option<String>,
    /// Relay binding: `auto` (gRPC, falling back to WebSocket), `grpc`, or
    /// `ws`; also the link's `t=` hint.
    #[arg(
        long = "relay-transport",
        value_name = "BINDING",
        value_parser = ["auto", "grpc", "ws"],
        requires = "relay"
    )]
    pub(crate) relay_transport: Option<String>,
    /// PEM file of extra trusted CA certificates for the relay's TLS.
    #[arg(long = "relay-ca", value_name = "PEM", requires = "relay")]
    pub(crate) relay_ca: Option<std::path::PathBuf>,
    /// Use a throwaway relay identity instead of the database's identity
    /// file `<db>.relay-identity.json`: the link dies with the process.
    #[arg(long = "relay-ephemeral", requires = "relay")]
    pub(crate) relay_ephemeral: bool,
    /// Relay heartbeat interval in seconds (default 15; a stream silent for
    /// three intervals is presumed dead).
    #[arg(long = "relay-heartbeat", value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) relay_heartbeat: Option<u64>,
    /// Do not print the relay link (the daemon's output goes to its log
    /// file; `hya serve start` prints the link instead).
    #[arg(long = "relay-quiet-link", hide = true)]
    pub(crate) relay_quiet_link: bool,
}

impl RelayFlags {
    /// Whether any relay flag was given.
    pub(crate) fn is_set(&self) -> bool {
        self.relay.is_some() || self.relay_heartbeat.is_some()
    }
}

/// `hya serve relay …`: the running backend's relay connector, through the
/// loopback-only `RelayControl` rpcs.
#[derive(Subcommand, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ServeRelayAction {
    /// Join a relay (replacing any current one) and print the relay link.
    Connect {
        /// The relay's public URL (`https://host[:port][/prefix]` or
        /// `http://…`).
        proxy_url: String,
        /// Relay binding: `auto`, `grpc`, or `ws`.
        #[arg(long, value_name = "BINDING", value_parser = ["auto", "grpc", "ws"], default_value = "auto")]
        transport: String,
        /// PEM file of extra trusted CA certificates.
        #[arg(long = "relay-ca", value_name = "PEM")]
        relay_ca: Option<std::path::PathBuf>,
        /// A throwaway identity: the link dies with this connection.
        #[arg(long)]
        ephemeral: bool,
        /// Print `{status, link}` as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Leave the relay: the room is released and relay clients are cut off.
    Disconnect {
        /// Print the status as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show the relay connector's state (never the link's secret part).
    Status {
        /// Print the status as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the full relay link (a secret) on stdout.
    Link,
    /// Issue a new link key: every earlier link stops working and open relay
    /// connections are closed. Prints the new link.
    Rotate {
        /// Print `{link, status}` as JSON.
        #[arg(long)]
        json: bool,
    },
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
        /// Override global SQLite database path for this server (also after
        /// `start`, `status`, `stop`, `restart`).
        #[arg(long, global = true)]
        db: Option<String>,
        /// Join a secure relay (foreground `hya serve`).
        #[command(flatten)]
        relay: RelayFlags,
        /// Control the backend daemon of `--db` instead of serving in the
        /// foreground (default database: `$XDG_STATE_HOME/hya/sessions.db`).
        #[command(subcommand)]
        action: Option<ServeAction>,
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
    /// Run the relay proxy: a blind Noise rendezvous between a backend and a
    /// client (docs/relay.md). No config, providers, or database.
    Proxy {
        #[command(flatten)]
        args: ProxyArgs,
    },
    /// Reach a remote backend through a secure relay link: listen on a
    /// loopback port and carry every connection end-to-end encrypted to the
    /// backend (docs/relay.md "Connecting from a client"). Point a TUI at it
    /// with `--server <url> --remote`.
    Bridge {
        #[command(flatten)]
        args: crate::bridge::BridgeArgs,
    },
    /// Relay diagnostics.
    Relay {
        #[command(subcommand)]
        command: RelayCommand,
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

/// Bare `hya`'s `--backend <URL>`, validated: an `http(s)://` URL, and only
/// without a subcommand or `-p`.
pub(crate) fn bare_backend(cli: &Cli) -> anyhow::Result<Option<String>> {
    let Some(url) = &cli.backend else {
        return Ok(None);
    };
    if cli.command.is_some() || cli.prompt.is_some() {
        anyhow::bail!("--backend only applies to bare `hya`");
    }
    let trimmed = url.trim().trim_end_matches('/');
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://"))
        || trimmed.contains(char::is_whitespace)
    {
        anyhow::bail!("--backend needs an http:// or https:// URL, got {url:?}");
    }
    Ok(Some(trimmed.to_string()))
}

/// Bare `hya`'s `--connect [LINK]`: where the relay link comes from, and
/// the in-process bridge's `--relay-ca` / `--transport`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BareConnect {
    pub(crate) source: crate::bridge::LinkSource,
    pub(crate) relay_ca: Option<std::path::PathBuf>,
    pub(crate) transport: Option<hya_relay::link::Transport>,
}

/// Bare `hya`'s `--connect [LINK]` with its bridge flags. Only without a
/// subcommand or `-p` (`hya bridge` is the standalone form).
pub(crate) fn bare_connect(cli: &Cli) -> anyhow::Result<Option<BareConnect>> {
    let Some(value) = &cli.connect else {
        return Ok(None);
    };
    if cli.command.is_some() || cli.prompt.is_some() {
        anyhow::bail!(
            "--connect only applies to bare `hya`; use `hya bridge` for a standalone bridge"
        );
    }
    let transport = cli
        .connect_transport
        .as_deref()
        .map(crate::bridge::parse_transport)
        .transpose()?;
    Ok(Some(BareConnect {
        source: crate::bridge::LinkSource::from_arg(Some(value)),
        relay_ca: cli.connect_relay_ca.clone(),
        transport,
    }))
}

/// Bare `hya`'s `--resume [ID]` for the terminal TUI: a session id, or the
/// picker without one. Only without a subcommand or `-p`.
pub(crate) fn bare_resume(cli: &Cli) -> anyhow::Result<Option<crate::frontend::Resume>> {
    use crate::frontend::Resume;
    let Some(id) = &cli.resume else {
        return Ok(None);
    };
    if cli.command.is_some() || cli.prompt.is_some() {
        anyhow::bail!("--resume only applies to bare `hya` (the terminal TUI)");
    }
    let id = id.trim();
    Ok(Some(if id.is_empty() {
        Resume::Pick
    } else {
        Resume::Session(id.to_string())
    }))
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
    fn bare_hya_takes_resume_with_or_without_a_session_id() {
        use crate::frontend::Resume;
        let cli = parse(["hya", "--resume", "hysec_abcdefghijklmnopqrst"]);
        assert_eq!(
            super::bare_resume(&cli).unwrap(),
            Some(Resume::Session("hysec_abcdefghijklmnopqrst".into()))
        );
        assert_eq!(
            super::bare_resume(&parse(["hya", "--resume"])).unwrap(),
            Some(Resume::Pick)
        );
        // Before another flag it takes no id.
        let cli = parse(["hya", "--resume", "--port", "0"]);
        assert_eq!(super::bare_resume(&cli).unwrap(), Some(Resume::Pick));
        assert_eq!(cli.port, Some(0));
        assert_eq!(super::bare_resume(&parse(["hya"])).unwrap(), None);
        let error = super::bare_resume(&parse(["hya", "--resume", "x", "sessions"]))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("--resume only applies to bare `hya`"),
            "{error}"
        );
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
    fn parses_serve_daemon_actions_with_db_anywhere() {
        use super::ServeAction;
        let action = |cli: Cli| match cli.command {
            Some(super::Command::Serve { action, db, .. }) => (action, db.or(Some(cli.db))),
            _ => panic!("expected serve command"),
        };
        assert_eq!(
            action(parse(["hya", "serve", "start", "--json", "--db", "/s.db"])),
            (
                Some(ServeAction::Start {
                    json: true,
                    relay: super::RelayFlags::default()
                }),
                Some("/s.db".into())
            )
        );
        assert_eq!(
            action(parse(["hya", "serve", "--db", "/s.db", "status"])),
            (
                Some(ServeAction::Status { json: false }),
                Some("/s.db".into())
            )
        );
        assert_eq!(
            action(parse(["hya", "serve", "stop", "--force", "--timeout", "5"])).0,
            Some(ServeAction::Stop {
                force: true,
                timeout: 5
            })
        );
        assert_eq!(
            action(parse(["hya", "serve", "restart"])).0,
            Some(ServeAction::Restart {
                json: false,
                force: false,
                timeout: 30,
                relay: super::RelayFlags::default()
            })
        );
        // Plain `hya serve` still serves in the foreground.
        assert_eq!(
            action(parse(["hya", "serve", "--bind", "127.0.0.1:0"])).0,
            None
        );
    }

    #[test]
    fn parses_serve_relay_flags_and_relay_actions() {
        use super::{RelayFlags, ServeAction, ServeRelayAction};
        let serve = |cli: Cli| match cli.command {
            Some(super::Command::Serve { relay, action, .. }) => (relay, action),
            _ => panic!("expected serve command"),
        };
        let (relay, action) = serve(parse([
            "hya",
            "serve",
            "--relay",
            "https://relay.example.com/hya",
            "--relay-transport",
            "ws",
            "--relay-ephemeral",
        ]));
        assert_eq!(action, None);
        assert_eq!(
            relay.relay.as_deref(),
            Some("https://relay.example.com/hya")
        );
        assert_eq!(relay.relay_transport.as_deref(), Some("ws"));
        assert!(relay.relay_ephemeral);
        assert!(
            Cli::try_parse_from(["hya", "serve", "--relay-ephemeral"]).is_err(),
            "relay options need --relay"
        );
        let (_, action) = serve(parse([
            "hya",
            "serve",
            "start",
            "--relay",
            "http://100.64.0.7:8766",
        ]));
        assert_eq!(
            action,
            Some(ServeAction::Start {
                json: false,
                relay: RelayFlags {
                    relay: Some("http://100.64.0.7:8766".into()),
                    ..RelayFlags::default()
                }
            })
        );
        let (_, action) = serve(parse([
            "hya",
            "serve",
            "relay",
            "connect",
            "https://relay.example.com",
            "--transport",
            "grpc",
            "--db",
            "/s.db",
        ]));
        assert_eq!(
            action,
            Some(ServeAction::Relay {
                action: ServeRelayAction::Connect {
                    proxy_url: "https://relay.example.com".into(),
                    transport: "grpc".into(),
                    relay_ca: None,
                    ephemeral: false,
                    json: false,
                }
            })
        );
        for (words, expected) in [
            ("status", ServeRelayAction::Status { json: false }),
            ("link", ServeRelayAction::Link),
            ("rotate", ServeRelayAction::Rotate { json: false }),
            ("disconnect", ServeRelayAction::Disconnect { json: false }),
        ] {
            let (_, action) = serve(parse(["hya", "serve", "relay", words]));
            assert_eq!(
                action,
                Some(ServeAction::Relay { action: expected }),
                "{words}"
            );
        }
    }

    #[test]
    fn backend_is_a_bare_hya_url() {
        let cli = parse(["hya", "--backend", "http://127.0.0.1:4096/"]);
        assert_eq!(
            super::bare_backend(&cli).unwrap().as_deref(),
            Some("http://127.0.0.1:4096")
        );
        assert_eq!(super::bare_backend(&parse(["hya"])).unwrap(), None);
        let error = super::bare_backend(&parse(["hya", "--backend", "localhost:4096"]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("http://"), "{error}");
        let error = super::bare_backend(&parse(["hya", "--backend", "http://x", "sessions"]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("only applies to bare"), "{error}");
    }

    #[test]
    fn bare_connect_takes_the_bridge_relay_ca_and_transport() {
        use hya_relay::link::Transport;
        let bare = super::bare_connect(&parse([
            "hya",
            "--connect",
            "-",
            "--relay-ca",
            "/etc/relay-ca.pem",
            "--transport",
            "ws",
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(
            bare.relay_ca.as_deref(),
            Some(std::path::Path::new("/etc/relay-ca.pem"))
        );
        assert_eq!(bare.transport, Some(Transport::Ws));
        // A bare `--connect` alone: neither.
        let bare = super::bare_connect(&parse(["hya", "--connect", "-"]))
            .unwrap()
            .unwrap();
        assert_eq!((bare.relay_ca, bare.transport), (None, None));
        // Both need `--connect`; the binding is one of the three.
        assert!(Cli::try_parse_from(["hya", "--relay-ca", "/x.pem"]).is_err());
        assert!(Cli::try_parse_from(["hya", "--transport", "ws"]).is_err());
        assert!(Cli::try_parse_from(["hya", "--connect", "-", "--transport", "quic"]).is_err());
    }

    #[test]
    fn connect_is_a_bare_hya_link_source() {
        use crate::bridge::LinkSource;
        let source = |cli| super::bare_connect(&cli).unwrap().map(|bare| bare.source);
        let link = "hya://relay.example.com/eh7ddx5bksrgcytl7bkai36se4#k.p";
        assert_eq!(
            source(parse(["hya", "--connect", link])),
            Some(LinkSource::Arg(link.into()))
        );
        assert_eq!(
            source(parse(["hya", "--connect", "-"])),
            Some(LinkSource::Stdin)
        );
        // Without a value: `$HYA_RELAY_LINK`.
        assert_eq!(source(parse(["hya", "--connect"])), Some(LinkSource::Env));
        assert_eq!(super::bare_connect(&parse(["hya"])).unwrap(), None);
        assert!(
            Cli::try_parse_from(["hya", "--connect", "-", "--backend", "http://x"]).is_err(),
            "--connect conflicts with --backend"
        );
        let error = super::bare_connect(&parse(["hya", "--connect", "-", "sessions"]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("only applies to bare"), "{error}");
        // The standalone form parses as a subcommand.
        let cli = parse(["hya", "bridge", "-", "--json", "--listen", "127.0.0.1:0"]);
        assert!(matches!(cli.command, Some(super::Command::Bridge { .. })));
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
