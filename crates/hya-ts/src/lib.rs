//! `hya-ts` launcher CLI parsing and Bun TUI command construction.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = invocation_name(), version, about = "hya TypeScript terminal frontend")]
pub struct Cli {
    /// Auth / OAuth subcommands (when absent, launch the TypeScript TUI).
    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(value_name = "PROJECT")]
    pub project: Option<PathBuf>,
    #[arg(long, value_name = "URL", value_parser = parse_server_url)]
    pub server: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub backend_bin: Option<PathBuf>,
    #[arg(long, value_name = "PATH", default_value = "bun")]
    pub bun: PathBuf,
    #[arg(long, value_name = "SOURCE")]
    pub import: Option<String>,
    #[arg(long)]
    pub r#continue: bool,
    #[arg(long, value_name = "ID")]
    pub session: Option<String>,
    #[arg(long)]
    pub fork: bool,
    #[arg(long, value_name = "TEXT")]
    pub prompt: Option<String>,
    #[arg(long, value_name = "NAME")]
    pub agent: Option<String>,
    #[arg(long, value_name = "PROVIDER/MODEL")]
    pub model: Option<String>,
}

pub fn invocation_name() -> &'static str {
    match std::env::args_os()
        .next()
        .as_deref()
        .and_then(|arg| Path::new(arg).file_name())
    {
        Some(name) if name == "hya" => "hya",
        _ => "hya-ts",
    }
}

/// Top-level subcommands available on `hya-ts` in addition to the default TUI launch.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Interactive OAuth login / status for openai-codex and grok-build.
    Oauth {
        #[command(subcommand)]
        command: OauthCommand,
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
}

/// OAuth subcommands — same surface as `hya-backend oauth`.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum OauthCommand {
    /// Run an interactive OAuth login for openai-codex or grok-build.
    Login {
        /// Provider id to write under `providers.<id>` and `auth/<id>.yaml`.
        #[arg(long)]
        provider: String,
        /// OAuth provider type: `openai-codex` or `grok-build`.
        #[arg(long = "type", value_name = "TYPE")]
        oauth_type: String,
        /// Use the device-code flow (default for openai-codex and grok-build).
        #[arg(long)]
        device: bool,
        /// openai-codex only: use localhost PKCE callback instead of Codex device-code.
        #[arg(long)]
        loopback: bool,
        /// Print the verification URL without opening a browser
        /// (default for openai-codex device login, matching Codex CLI).
        #[arg(long)]
        no_browser: bool,
        /// Open a system browser for the verification / authorize URL.
        #[arg(long)]
        browser: bool,
        /// Model id to register on the provider (default depends on type).
        #[arg(long)]
        model: Option<String>,
        /// Override the inference base URL (defaults depend on type).
        #[arg(long)]
        base_url: Option<String>,
    },
    /// Show saved auth status (OAuth type and expiry; no secrets).
    Status {
        /// Optional provider id filter.
        provider: Option<String>,
    },
}

/// Token list/logout — same surface as `hya-backend auth`.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum AuthCommand {
    /// List provider ids with saved auth tokens.
    List,
    /// Remove a saved provider auth token.
    Logout {
        /// Provider id as it appears in your hya config.
        provider: String,
    },
}

impl Cli {
    /// Validate argument relationships before any process starts.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.command.is_some() && self.import.is_some() {
            return Err("--import cannot be used with a subcommand");
        }
        if self.command.is_some() {
            return Ok(());
        }
        if self.fork && !self.r#continue && self.session.is_none() {
            return Err("--fork requires --continue or --session");
        }
        Ok(())
    }
}

/// Build argv for a sibling `hya-backend` auth/oauth invocation.
///
/// Auth lives in the backend today; `hya-ts` exposes the same commands and
/// forwards them so users can log in without switching binaries.
#[must_use]
pub fn backend_auth_args(command: &Command) -> Vec<OsString> {
    match command {
        Command::Oauth {
            command:
                OauthCommand::Login {
                    provider,
                    oauth_type,
                    device,
                    loopback,
                    no_browser,
                    browser,
                    model,
                    base_url,
                },
        } => {
            let mut args = vec![
                OsString::from("oauth"),
                OsString::from("login"),
                OsString::from("--provider"),
                OsString::from(provider),
                OsString::from("--type"),
                OsString::from(oauth_type),
            ];
            if *device {
                args.push(OsString::from("--device"));
            }
            if *loopback {
                args.push(OsString::from("--loopback"));
            }
            if *no_browser {
                args.push(OsString::from("--no-browser"));
            }
            if *browser {
                args.push(OsString::from("--browser"));
            }
            if let Some(model) = model {
                args.push(OsString::from("--model"));
                args.push(OsString::from(model));
            }
            if let Some(base_url) = base_url {
                args.push(OsString::from("--base-url"));
                args.push(OsString::from(base_url));
            }
            args
        }
        Command::Oauth {
            command: OauthCommand::Status { provider },
        } => {
            let mut args = vec![OsString::from("oauth"), OsString::from("status")];
            if let Some(provider) = provider {
                args.push(OsString::from(provider));
            }
            args
        }
        Command::Login { provider, token } => {
            vec![
                OsString::from("login"),
                OsString::from(provider),
                OsString::from(token),
            ]
        }
        Command::Auth {
            command: AuthCommand::List,
        } => vec![OsString::from("auth"), OsString::from("list")],
        Command::Auth {
            command: AuthCommand::Logout { provider },
        } => vec![
            OsString::from("auth"),
            OsString::from("logout"),
            OsString::from(provider),
        ],
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct BunCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub current_dir: PathBuf,
}

/// Build the attached-mode Bun command using the process current directory.
pub fn build_bun_command(cli: &Cli, runtime_dir: &Path) -> Result<BunCommand, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    build_bun_command_from(cli, runtime_dir, &cwd)
}

/// Build the attached-mode Bun command with an explicit current directory (for tests).
pub fn build_bun_command_from(
    cli: &Cli,
    runtime_dir: &Path,
    cwd: &Path,
) -> Result<BunCommand, String> {
    let url = cli
        .server
        .as_deref()
        .ok_or_else(|| "--server is required to construct an attached command".to_string())?;
    build_bun_command_with_url(cli, runtime_dir, cwd, url)
}

fn build_bun_command_with_url(
    cli: &Cli,
    runtime_dir: &Path,
    cwd: &Path,
    url: &str,
) -> Result<BunCommand, String> {
    let project = cli
        .project
        .as_deref()
        .unwrap_or(cwd)
        .canonicalize()
        .map_err(|error| {
            format!(
                "cannot resolve project {}: {error}",
                cli.project.as_deref().unwrap_or(cwd).display()
            )
        })?;
    let runtime_dir = runtime_dir.canonicalize().map_err(|error| {
        format!(
            "cannot resolve TUI runtime {}: {error}",
            runtime_dir.display()
        )
    })?;
    let mut args = vec![
        OsString::from("src/main.tsx"),
        OsString::from("--url"),
        OsString::from(url),
        OsString::from("--project"),
        project.into_os_string(),
    ];
    if cli.r#continue {
        args.push("--continue".into());
    }
    push_value(&mut args, "--session", cli.session.as_deref());
    if cli.fork {
        args.push("--fork".into());
    }
    push_value(&mut args, "--prompt", cli.prompt.as_deref());
    push_value(&mut args, "--agent", cli.agent.as_deref());
    push_value(&mut args, "--model", cli.model.as_deref());
    Ok(BunCommand {
        program: cli.bun.clone(),
        args,
        current_dir: runtime_dir,
    })
}

fn push_value(args: &mut Vec<OsString>, flag: &str, value: Option<&str>) {
    if let Some(value) = value {
        args.push(flag.into());
        args.push(value.into());
    }
}

/// Resolve runtime assets in development override, installed, then workspace order.
pub fn resolve_runtime_dir(
    override_dir: Option<&OsStr>,
    executable: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, String> {
    let installed = executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("../lib/hya/hya-tui-ts");
    let candidates = [
        override_dir.map(PathBuf::from),
        Some(installed),
        Some(workspace_root.join("packages/hya-tui-ts")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find_map(|candidate| candidate.canonicalize().ok())
        .ok_or_else(|| {
            "cannot locate hya-tui-ts; set HYA_TUI_TS_DIR or install it under ../lib/hya/hya-tui-ts"
                .to_string()
        })
}

/// Resolve the `hya-backend` binary used for auth commands and auto-serve.
pub fn resolve_backend_bin(
    backend_bin: Option<&Path>,
    env_backend: Option<&OsStr>,
    executable: &Path,
    workspace_root: &Path,
) -> PathBuf {
    if let Some(path) = backend_bin {
        return path.to_path_buf();
    }
    if let Some(path) = env_backend {
        return PathBuf::from(path);
    }
    let sibling = executable.with_file_name("hya-backend");
    if sibling.is_file() {
        return sibling;
    }
    for profile in ["release", "debug"] {
        let candidate = workspace_root
            .join("target")
            .join(profile)
            .join("hya-backend");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("hya-backend")
}

fn parse_server_url(value: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(value).map_err(|error| format!("invalid server URL: {error}"))?;
    match url.scheme() {
        "http" | "https" if url.host().is_some() => Ok(value.to_string()),
        _ => Err("server URL must use http or https and include a host".to_string()),
    }
}
