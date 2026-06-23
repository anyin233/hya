use clap::{Parser, Subcommand};

use crate::auth_cmd::AuthCommand;

#[derive(Parser)]
#[command(
    name = "yaca",
    version,
    about = "yaca — a multi-agent coding agent",
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
    /// Model id to use (overrides config `default_model` + `YACA_MODEL`).
    #[arg(long, global = true, value_name = "MODEL")]
    pub(crate) model: Option<String>,
    /// Auto-approve every tool action (edit/write/shell anywhere). Use with care.
    #[arg(long, global = true)]
    pub(crate) yolo: bool,
    /// Start the minimal interactive interface.
    #[arg(long)]
    pub(crate) mini: bool,
    /// SQLite database for the interactive TUI (empty = in-memory).
    #[arg(long, default_value = "")]
    pub(crate) db: String,
    /// Resume an existing session id in the interactive TUI.
    #[arg(long)]
    pub(crate) resume: Option<String>,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

impl Cli {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.mini && self.command.is_some() {
            return Err("--mini must be used without a subcommand");
        }
        Ok(())
    }
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// OpenCode-compatible alias for headless prompt execution.
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
        /// Hostname to listen on. OpenCode-compatible alias for the host part of `--bind`.
        #[arg(long)]
        hostname: Option<String>,
        /// Port to listen on. OpenCode-compatible alias for the port part of `--bind`.
        #[arg(long)]
        port: Option<u16>,
        /// SQLite database path. Empty string uses an in-memory store.
        #[arg(long, default_value = "")]
        db: String,
    },
    /// Replay a session's event log from a database as JSON lines.
    TailSession {
        /// Session id (UUID).
        id: String,
        /// SQLite database path the session was written to.
        #[arg(long)]
        db: String,
    },
    /// Save an auth token for a provider id (used instead of an inline api_key).
    Login {
        /// Provider id as it appears in your yaca config.
        provider: String,
        /// The bearer/API token to store.
        token: String,
    },
    /// Inspect or remove saved provider auth tokens.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// List sessions stored in a database.
    Sessions {
        /// SQLite database path.
        #[arg(long)]
        db: String,
    },
    /// JSONL RPC over stdin/stdout: read {"type":"prompt","text":...} lines, emit event JSONL.
    Rpc,
}

pub(crate) fn serve_bind(bind: String, hostname: Option<String>, port: Option<u16>) -> String {
    if hostname.is_none() && port.is_none() {
        return bind;
    }
    let (default_host, default_port) = bind.rsplit_once(':').unwrap_or((&bind, "8080"));
    let host = hostname.unwrap_or_else(|| default_host.to_string());
    let port = port.map_or_else(|| default_port.to_string(), |port| port.to_string());
    format!("{host}:{port}")
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;
    use clap::Parser as _;

    use super::Cli;

    fn parse<const N: usize>(args: [&str; N]) -> Cli {
        match Cli::try_parse_from(args) {
            Ok(cli) => cli,
            Err(err) => panic!("{err}"),
        }
    }

    #[test]
    fn parses_opencode_mini_alias() {
        let cli = parse(["yaca", "--mini"]);
        assert!(cli.mini);
        assert!(cli.command.is_none());
        assert!(cli.validate().is_ok());
    }

    #[test]
    fn rejects_mini_with_subcommand() {
        let cli = parse(["yaca", "--mini", "exec", "hello"]);
        let Err(error) = cli.validate() else {
            panic!("mini with subcommand should be rejected");
        };
        assert_eq!(error, "--mini must be used without a subcommand");
    }

    #[test]
    fn help_exposes_mini_alias() {
        let help = Cli::command().render_help().to_string();
        assert!(help.contains("--mini"));
    }

    #[test]
    fn parses_opencode_run_alias() {
        let cli = parse(["yaca", "run", "--format", "json", "hello", "world"]);
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
    fn parses_opencode_serve_network_aliases() {
        let cli = parse(["yaca", "serve", "--hostname", "0.0.0.0", "--port", "4096"]);
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
                assert_eq!(super::serve_bind(bind, hostname, port), "0.0.0.0:4096");
            }
            _ => panic!("expected serve command"),
        }
    }
}
