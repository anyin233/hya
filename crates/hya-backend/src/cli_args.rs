use clap::{Parser, Subcommand};

use crate::agent_cmd::AgentCommand;
use crate::auth_cmd::OauthCommand;
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
    /// Accept this Host name (a host name or IP address, no port) besides
    /// `localhost`, `127.0.0.1`, and `[::1]` on the backend daemon bare
    /// `hya` starts (repeatable; the Host allowlist, docs/cli.md "Allowed
    /// Host names"). Only for bare `hya` without `--backend`/`--connect`.
    #[arg(
        long = "allow-host",
        value_name = "HOST",
        conflicts_with_all = ["backend", "connect"]
    )]
    pub(crate) allow_host: Vec<String>,
    /// Open this session in the terminal TUI and unarchive it; without an
    /// id, pick one of the directory's sessions (archived ones included).
    /// Only for bare `hya`.
    #[arg(long, value_name = "ID", num_args = 0..=1, default_missing_value = "")]
    pub(crate) resume: Option<String>,
    /// Model id to use (overrides config `default_model` + `HYA_MODEL`).
    #[arg(long, global = true, value_name = "MODEL")]
    pub(crate) model: Option<String>,
    /// Reasoning effort for the selected model, encoded as a `#effort` suffix.
    #[arg(long, global = true, value_name = "EFFORT")]
    pub(crate) effort: Option<String>,
    /// Auto-approve every tool action (edit/write/shell anywhere). Use with care.
    #[arg(long, global = true)]
    pub(crate) yolo: bool,
    /// Run in bare mode (`BuiltinBundleProfile::Bare`): load zero built-in
    /// bundles, using only the 8 standard bare tools (`read`, `write`, `edit`,
    /// `bash`, `grep`, `todo`, `rewrite`, `skill`), core `code-mode` (without
    /// `agent()`), `AGENTS.md`, skills, MCP, user/project bundles & plugins,
    /// and the minimal `hya-bare` default agent.
    #[arg(long, global = true)]
    pub(crate) bare: bool,
    /// Skip external `AGENTS.md` context, MCP servers, disk skills, and plugins.
    /// Combine with `--bare` (`--bare --pure`) for pure kernel benchmarks.
    #[arg(long, global = true)]
    pub(crate) pure: bool,
    /// SQLite database path. Applies to headless exec persistence, serve,
    /// bare `hya`, sessions, and replay. Empty: an in-memory store for
    /// exec/serve; `$XDG_STATE_HOME/hya/sessions.db` for bare `hya`,
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
        #[arg(long, default_value_t = 45, value_name = "SECONDS")]
        timeout: u64,
    },
    /// Stop the backend of `--db` (if one runs), then start a new daemon.
    ///
    Restart {
        /// Print the new daemon as JSON (like `start --json`).
        #[arg(long)]
        json: bool,
        /// Kill the old backend when it has not stopped within `--timeout`.
        #[arg(long)]
        force: bool,
        /// Seconds to wait for the old backend to stop.
        #[arg(long, default_value_t = 45, value_name = "SECONDS")]
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

// Parsed once per process; boxing a variant would only add indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
pub(crate) enum Command {
    /// Serve an ACP v1 editor over standard input and output.
    Acp,
    /// Run a single prompt headlessly and print the resulting transcript.
    Exec {
        /// The user prompt to send to the agent. Use `-` to read it from stdin.
        prompt: String,
        /// Emit the event stream as JSONL instead of a rendered transcript.
        #[arg(long)]
        json: bool,
        /// Include durable events from every descendant in the local JSONL stream.
        #[arg(long, requires = "json")]
        include_subagents: bool,
        /// Wall-clock budget in seconds for in-process runs, including --bare.
        /// Enables deadline notices and caps tool timeouts. The outside harness
        /// enforces the process deadline.
        #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(u64).range(1..))]
        time_budget: Option<u64>,
    },
    /// Start the HTTP + SSE server.
    Serve {
        /// Address to bind. Use `127.0.0.1:0` for an ephemeral port.
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: String,
        /// Accept requests whose Host names HOST (a host name or IP
        /// address, no port) besides `localhost`, `127.0.0.1`, and `[::1]`
        /// (repeatable; also after `start`/`restart`). Every other Host is
        /// refused with 403 (DNS rebinding); a non-wildcard `--bind` host is
        /// accepted by itself, a wildcard bind (`0.0.0.0`) needs
        /// the LAN names here.
        #[arg(long = "allow-host", value_name = "HOST", global = true)]
        allow_host: Vec<String>,
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
    /// Add, list, or remove model providers and their saved keys (`hya
    /// provider add` walks through base URL, protocol, and API key, and
    /// checks the model list).
    #[command(alias = "providers")]
    Provider {
        #[command(subcommand)]
        command: crate::provider_cmd::ProviderCommand,
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
        /// Include child sessions and archived sessions.
        #[arg(long, conflicts_with = "archived")]
        all: bool,
        /// List only archived root sessions.
        #[arg(long)]
        archived: bool,
        /// Archive or unarchive a session instead of listing.
        #[command(subcommand)]
        action: Option<crate::sessions_cmd::SessionsAction>,
    },
    /// Update hya: bare `hya update` reinstalls the latest GitHub release into
    /// this hya's prefix (docs/getting-started/installation.md); `hya update
    /// version` prints the updater's package version.
    #[command(args_conflicts_with_subcommands = true)]
    Update {
        #[command(subcommand)]
        command: Option<hya_updater::cli::UpdateCommand>,
        #[command(flatten)]
        install: hya_updater::release_install::ReleaseInstallArgs,
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

/// Default WebUI port of bare `hya` (`--port` overrides it).
pub(crate) const DEFAULT_WEB_PORT: u16 = 3250;

/// The WebUI port of bare `hya`: `--port`, else [`DEFAULT_WEB_PORT`].
///
/// `--port` is a top-level flag of the bare invocation only; with a
/// subcommand or `-p` goal mode it is an error rather than silently ignored
/// (`hya serve --bind` sets the server's address).
pub(crate) fn bare_web_port(cli: &Cli) -> anyhow::Result<u16> {
    if cli.port.is_some() && (cli.command.is_some() || cli.prompt.is_some()) {
        anyhow::bail!(
            "--port only applies to bare `hya` (the WebUI port); use `hya serve --bind` for the server"
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

/// Bare `hya`'s `--allow-host` names (normalized) for the daemon it starts.
/// Only without a subcommand or `-p`.
pub(crate) fn bare_allow_hosts(cli: &Cli) -> anyhow::Result<Vec<String>> {
    if cli.allow_host.is_empty() {
        return Ok(Vec::new());
    }
    if cli.command.is_some() || cli.prompt.is_some() {
        anyhow::bail!("--allow-host applies to bare `hya` and `hya serve`; put it after `serve`");
    }
    allow_host_names(&cli.allow_host)
}

/// `--allow-host` values checked and normalized (lowercase, IPv6 in
/// brackets), loopback names dropped.
pub(crate) fn allow_host_names(names: &[String]) -> anyhow::Result<Vec<String>> {
    hya_server::HostPolicy::with_hosts(names)
        .map(|policy| policy.extra_hosts())
        .map_err(anyhow::Error::msg)
}

/// The Host allowlist of `hya serve --bind <bind> --allow-host …`: the
/// `--allow-host` names, plus the bind host unless it is a wildcard
/// (`0.0.0.0`, `::`) or loopback.
pub(crate) fn serve_host_policy(
    bind: &str,
    allow_hosts: &[String],
) -> anyhow::Result<hya_server::HostPolicy> {
    let host = bind
        .rsplit_once(':')
        .map_or(bind, |(host, _port)| host)
        .trim();
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    let wildcard = bare.is_empty()
        || bare == "*"
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_unspecified());
    let mut names = allow_hosts.to_vec();
    if !wildcard && hya_server::HostPolicy::with_hosts([bare]).is_ok() {
        names.push(bare.to_owned());
    }
    hya_server::HostPolicy::with_hosts(&names).map_err(anyhow::Error::msg)
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

/// Merge a validated `--effort` flag into a model reference.
pub(crate) fn merge_model_effort(
    model: Option<String>,
    effort: Option<String>,
) -> anyhow::Result<Option<String>> {
    let Some(effort) = effort
        .map(|value| value.trim().to_string())
        .filter(|v| !v.is_empty())
    else {
        return Ok(model);
    };
    hya_provider::ReasoningEffort::parse(&effort)
        .ok_or_else(|| anyhow::anyhow!("invalid effort `{effort}`"))?;
    let Some(model) = model else {
        anyhow::bail!("--effort requires --model");
    };
    if model.contains('#') {
        anyhow::bail!("--effort conflicts with a model effort suffix");
    }
    Ok(Some(format!("{model}#{effort}")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use super::{Cli, Command, RelayFlags, ServeAction, ServeRelayAction};

    fn p(cmd: &str) -> Cli {
        Cli::try_parse_from(cmd.split_whitespace()).unwrap_or_else(|e| panic!("{cmd}: {e}"))
    }

    fn reject(cmds: &[&str]) {
        for cmd in cmds {
            assert!(
                Cli::try_parse_from(cmd.split_whitespace()).is_err(),
                "{cmd}"
            );
        }
    }

    #[test]
    fn cli_metadata_help_and_rejected_legacy_flags() {
        assert_eq!(Cli::command().get_name(), "hya");
        let help = Cli::command().render_long_help().to_string();
        assert!(!help.contains("--mini"));
        for needle in ["--port <PORT>", "3250", "WebUI"] {
            assert!(help.contains(needle), "{help}");
        }
        assert_eq!(
            Cli::try_parse_from(["hya", "--mini"]).err().unwrap().kind(),
            clap::error::ErrorKind::UnknownArgument
        );
        reject(&["hya auth list"]);
    }

    #[test]
    fn parses_update_and_bundle_subcommands_and_rejects_invalid_combinations() {
        use crate::bundle_cmd::BundleCommand;
        use hya_updater::cli::UpdateCommand;

        assert!(matches!(
            p("hya update --version v1.2.3 --force").command,
            Some(Command::Update { command: None, ref install })
                if install.version.as_deref() == Some("v1.2.3") && install.force
        ));
        assert!(matches!(
            p("hya update version").command,
            Some(Command::Update {
                command: Some(UpdateCommand::Version),
                ..
            })
        ));
        reject(&[
            "hya update --force version",
            "hya update --tui-only",
            "hya update --backend-only",
            "hya update tui",
            "hya update status --root /tmp/u",
            "hya update apply --root /tmp/u",
            "hya update init-roots --root /tmp/u",
        ]);
        for verb in ["remove", "uninstall"] {
            assert!(matches!(
                p(&format!("hya bundle {verb} -y --project hya/demo")).command,
                Some(Command::Bundle {
                    command: BundleCommand::Remove { .. }
                })
            ));
        }
        for ok in [
            "hya bundle install -y demo.hyabundle",
            "hya bundle install --yes --user demo.hyabundle",
            "hya bundle install --project demo.hyabundle",
            "hya bundle pack ./my-bundle out.hyabundle",
            "hya bundle verify --project --overwrite demo.hyabundle",
            "hya bundle verify --strict --protocol-smoke ./my-bundle",
            "hya bundle list --project",
            "hya bundle list --user",
            "hya bundle info --project hya/demo",
            "hya bundle info -f demo.hyabundle",
            "hya bundle info demo.hyabundle",
            "hya bundle search --project goal-loop",
            "hya bundle schema --user hya/demo",
        ] {
            p(ok);
        }
        reject(&[
            "hya bundle install --user --project demo.hyabundle",
            "hya bundle verify -y demo.hyabundle",
            "hya bundle search",
            "hya bundle schemas",
        ]);
        assert!(Cli::try_parse_from(["hya", "bundle", "search", "   "]).is_err());
    }

    #[test]
    fn bare_hya_port_resume_backend_connect_and_allow_host_validation() {
        use crate::bridge::LinkSource;
        use crate::frontend::Resume;
        use hya_relay::link::Transport;

        assert_eq!(super::bare_web_port(&p("hya")).unwrap(), 3250);
        assert_eq!(super::bare_web_port(&p("hya --port 0")).unwrap(), 0);
        reject(&["hya --port 70000", "hya --port web"]);
        for sub in ["hya --port 9000 sessions", "hya --port 9000 -p goal"] {
            assert!(
                super::bare_web_port(&p(sub))
                    .unwrap_err()
                    .to_string()
                    .contains("--port only applies to bare `hya`")
            );
        }
        assert_eq!(
            super::bare_resume(&p("hya --resume hysec_abcdefghijklmnopqrst")).unwrap(),
            Some(Resume::Session("hysec_abcdefghijklmnopqrst".into()))
        );
        assert_eq!(
            super::bare_resume(&p("hya --resume --port 0")).unwrap(),
            Some(Resume::Pick)
        );
        assert!(super::bare_resume(&p("hya --resume x sessions")).is_err());

        assert_eq!(
            super::bare_backend(&p("hya --backend http://127.0.0.1:4096/"))
                .unwrap()
                .as_deref(),
            Some("http://127.0.0.1:4096")
        );
        assert!(super::bare_backend(&p("hya --backend localhost:4096")).is_err());
        assert!(super::bare_backend(&p("hya --backend http://x sessions")).is_err());

        let link = "hya://relay.example.com/eh7ddx5bksrgcytl7bkai36se4#k.p";
        let bare = super::bare_connect(&p(
            "hya --connect - --relay-ca /etc/relay-ca.pem --transport ws",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(
            (bare.source, bare.transport, bare.relay_ca.as_deref()),
            (
                LinkSource::Stdin,
                Some(Transport::Ws),
                Some(std::path::Path::new("/etc/relay-ca.pem"))
            )
        );
        assert_eq!(
            super::bare_connect(&p(&format!("hya --connect {link}")))
                .unwrap()
                .unwrap()
                .source,
            LinkSource::Arg(link.into())
        );
        assert_eq!(
            super::bare_connect(&p("hya --connect"))
                .unwrap()
                .unwrap()
                .source,
            LinkSource::Env
        );
        assert!(super::bare_connect(&p("hya --connect - sessions")).is_err());
        reject(&[
            "hya --relay-ca /x.pem",
            "hya --transport ws",
            "hya --connect - --transport quic",
            "hya --connect - --backend http://x",
            "hya --allow-host x --backend http://h:1",
        ]);
        assert_eq!(
            super::bare_allow_hosts(&p("hya --allow-host hya.lan")).unwrap(),
            ["hya.lan"]
        );
    }

    #[test]
    fn parses_serve_actions_relay_flags_and_host_policy() {
        let serve_of = |cmd: &str| match p(cmd).command {
            Some(Command::Serve {
                action,
                db,
                relay,
                allow_host,
                ..
            }) => (action, db, relay, allow_host),
            _ => panic!("expected serve"),
        };
        let (action, db, _, _) = serve_of("hya serve start --json --db /s.db");
        assert_eq!(
            action,
            Some(ServeAction::Start {
                json: true,
                relay: RelayFlags::default()
            })
        );
        assert_eq!(db.as_deref(), Some("/s.db"));
        assert_eq!(
            serve_of("hya serve stop --force --timeout 5").0,
            Some(ServeAction::Stop {
                force: true,
                timeout: 5
            })
        );
        assert_eq!(
            serve_of("hya serve restart").0,
            Some(ServeAction::Restart {
                json: false,
                force: false,
                timeout: 45,
                relay: RelayFlags::default()
            })
        );
        let (action, _, relay, _) = serve_of(
            "hya serve --relay https://relay.example.com/hya --relay-transport ws --relay-ephemeral",
        );
        assert!(
            action.is_none()
                && relay.relay_transport.as_deref() == Some("ws")
                && relay.relay_ephemeral
        );
        reject(&["hya serve --relay-ephemeral"]);

        for (words, expected) in [
            ("status", ServeRelayAction::Status { json: false }),
            ("link", ServeRelayAction::Link),
            ("rotate", ServeRelayAction::Rotate { json: false }),
            ("disconnect", ServeRelayAction::Disconnect { json: false }),
        ] {
            assert_eq!(
                serve_of(&format!("hya serve relay {words}")).0,
                Some(ServeAction::Relay { action: expected })
            );
        }
        assert!(matches!(
            serve_of("hya serve relay connect https://relay.example.com --transport grpc").0,
            Some(ServeAction::Relay {
                action: ServeRelayAction::Connect { .. }
            })
        ));
        let (_, _, _, allow) =
            serve_of("hya serve --allow-host Hya.Example.Lan --allow-host 192.168.1.20");
        assert_eq!(
            super::allow_host_names(&allow).unwrap(),
            ["192.168.1.20", "hya.example.lan"]
        );
        assert!(super::allow_host_names(&["host:8080".into()]).is_err());

        let hosts = |bind: &str| super::serve_host_policy(bind, &[]).unwrap().extra_hosts();
        for empty in ["127.0.0.1:8080", "0.0.0.0:8080", "[::]:8080"] {
            assert!(hosts(empty).is_empty());
        }
        assert_eq!(hosts("192.168.1.5:8080"), ["192.168.1.5"]);
        assert_eq!(hosts("[fe80::1]:8080"), ["[fe80::1]"]);
        assert_eq!(hosts("MyBox:8080"), ["mybox"]);
        let policy = super::serve_host_policy("0.0.0.0:8080", &["hya.lan".into()]).unwrap();
        assert!(policy.allows("hya.lan:8080") && !policy.allows("192.168.1.5:8080"));
    }

    #[test]
    fn parses_goal_and_loop_flags() {
        assert_eq!(
            p("hya -p ship --evaluator-model deep/o3-mini")
                .evaluator_model
                .as_deref(),
            Some("deep/o3-mini")
        );
        assert!(p("hya -p ship").evaluator_model.is_none());
        assert!(matches!(
            p("hya loop --target green --budget 5 --while true --evaluator-model deep/o3-mini").command,
            Some(Command::Loop { ref target, budget: Some(5), max_iterations: None, while_command: Some(ref w), until_command: None, evaluator_model: Some(ref e) })
                if target == "green" && w == "true" && e == "deep/o3-mini"
        ));
        assert!(matches!(
            p("hya loop --target docs --until false --max-iterations 3").command,
            Some(Command::Loop {
                budget: None,
                max_iterations: Some(3),
                until_command: Some(_),
                ..
            })
        ));
    }

    #[test]
    fn parses_exec_workflow_sessions_models_provider_oauth_and_effort() {
        assert_eq!(
            super::merge_model_effort(Some("openai/gpt".into()), Some("high".into())).unwrap(),
            Some("openai/gpt#high".into())
        );
        assert!(
            super::merge_model_effort(Some("openai/gpt#low".into()), Some("high".into())).is_err()
        );
        assert!(
            super::merge_model_effort(Some("openai/gpt".into()), Some("bogus".into())).is_err()
        );
        reject(&[
            "hya exec --time-budget 0 hi",
            "hya exec --time-budget -1 hi",
            "hya exec --time-budget 1.5 hi",
            "hya workflow use echo-input",
        ]);
        assert!(matches!(
            p("hya exec --json --time-budget 30 go").command,
            Some(Command::Exec {
                json: true,
                time_budget: Some(30),
                ..
            })
        ));
        assert!(
            matches!(p("hya --db /a.db sessions --db /b.db").command, Some(Command::Sessions { db: Some(ref d), .. }) if d == "/b.db")
        );
        assert!(
            matches!(p("hya models openai --verbose --refresh").command, Some(Command::Models { provider: Some(ref p), verbose: true, refresh: true }) if p == "openai")
        );
        assert!(matches!(
            p("hya providers list").command,
            Some(Command::Provider { .. })
        ));
        assert!(matches!(
            p("hya bridge - --json --listen 127.0.0.1:0").command,
            Some(Command::Bridge { .. })
        ));
        assert!(matches!(
            p("hya oauth login --provider codex --type openai-codex --loopback --browser --model gpt-5.3-codex").command,
            Some(Command::Oauth { command: crate::auth_cmd::OauthCommand::Login { ref provider, loopback: true, browser: true, no_browser: false, model: Some(ref m), .. } })
                if provider == "codex" && m == "gpt-5.3-codex"
        ));

        // Global `--bare` routes across interactive `hya`, `serve`, `serve start|restart`,
        // `exec`, and composes cleanly with `--pure` for kernel benchmarks.
        for cmd in [
            "hya --bare",
            "hya --bare --port 0",
            "hya --bare serve",
            "hya serve --bare",
            "hya serve --bare start",
            "hya serve start --bare",
            "hya serve restart --bare",
            "hya --bare exec hi",
            "hya exec --bare hi",
            "hya exec --bare --pure hi",
            "hya --bare --pure exec hi",
        ] {
            assert!(p(cmd).bare, "{cmd}");
        }
        let bare_pure = p("hya exec --bare --pure hi");
        assert!(bare_pure.bare && bare_pure.pure);
    }
}
