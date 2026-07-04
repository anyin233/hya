use clap::{Parser, Subcommand};

use crate::agent_cmd::AgentCommand;
use crate::auth_cmd::AuthCommand;

#[derive(Parser)]
#[command(
    name = "hya-backend",
    version,
    about = "hya — a multi-agent coding agent",
    long_about = None
)]
pub(crate) struct Cli {
    /// Headless goal mode: iterate the agent until an independent evaluator
    /// reports the goal met, or the iteration cap trips.
    #[arg(short = 'p', long = "prompt", value_name = "GOAL")]
    pub(crate) prompt: Option<String>,
    /// Iteration cap for `-p` goal mode.
    #[arg(long, default_value_t = 6)]
    pub(crate) max_iterations: u32,
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
    /// SQLite database path. Empty string uses an in-memory store. Applies to
    /// the interactive TUI, headless exec/run persistence, serve, sessions, and replay.
    #[arg(long, global = true, default_value = "")]
    pub(crate) db: String,
    /// Resume an existing session id in the interactive TUI.
    #[arg(long)]
    pub(crate) resume: Option<String>,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

impl Cli {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.resume.is_some() && self.prompt.is_some() {
            return Err("--resume must be used only for interactive startup");
        }
        if self.resume.is_some() && self.command.is_some() {
            return Err("--resume must be used without a subcommand");
        }
        Ok(())
    }
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
    /// List configured models.
    Models {
        /// Provider id to filter models by.
        provider: Option<String>,
        /// Accepted for Compat CLI compatibility.
        #[arg(long)]
        verbose: bool,
        /// Accepted for Compat CLI compatibility.
        #[arg(long)]
        refresh: bool,
    },
    /// List sessions stored in a database.
    Sessions {
        /// Override global SQLite database path for listing.
        #[arg(long)]
        db: Option<String>,
    },
    /// JSONL RPC over stdin/stdout: read {"type":"prompt","text":...} lines, emit event JSONL.
    Rpc,
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
mod tests {
    use clap::CommandFactory as _;
    use clap::Parser as _;

    use super::Cli;

    fn parse<const N: usize>(args: [&str; N]) -> Cli {
        Cli::try_parse_from(args).unwrap_or_else(|err| panic!("{err}"))
    }

    #[test]
    fn rejects_mini_as_unknown_argument() {
        let err = match Cli::try_parse_from(["hya-backend", "--mini"]) {
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
    fn rejects_resume_with_subcommand() {
        let cli = parse([
            "hya-backend",
            "--resume",
            "hysec_abcdefghijklmnopqrst",
            "exec",
            "hello",
        ]);

        assert_eq!(
            cli.validate(),
            Err("--resume must be used without a subcommand")
        );
    }

    #[test]
    fn rejects_resume_with_prompt_goal_mode() {
        let cli = parse([
            "hya-backend",
            "--resume",
            "hysec_abcdefghijklmnopqrst",
            "--prompt",
            "finish the task",
        ]);

        assert_eq!(
            cli.validate(),
            Err("--resume must be used only for interactive startup")
        );
    }

    #[test]
    fn parses_compat_run_alias() {
        let cli = parse(["hya-backend", "run", "--format", "json", "hello", "world"]);
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
    fn parses_compat_serve_network_aliases() {
        let cli = parse([
            "hya-backend",
            "serve",
            "--hostname",
            "0.0.0.0",
            "--port",
            "4096",
        ]);
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
            "hya-backend",
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
    fn parses_compat_models_command() {
        let cli = parse(["hya-backend", "models", "openai", "--verbose", "--refresh"]);
        match cli.command {
            Some(super::Command::Models {
                provider,
                verbose,
                refresh,
            }) => {
                assert_eq!(provider.as_deref(), Some("openai"));
                assert!(verbose);
                assert!(refresh);
            }
            _ => panic!("expected models command"),
        }
    }

    #[test]
    fn parses_compat_providers_alias_for_auth_list() {
        let cli = parse(["hya-backend", "providers", "list"]);
        match cli.command {
            Some(super::Command::Auth {
                command: crate::auth_cmd::AuthCommand::List,
            }) => {}
            _ => panic!("expected auth list command"),
        }
    }
}
