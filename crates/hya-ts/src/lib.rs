use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "hya-ts", about = "hya TypeScript terminal frontend")]
pub struct Cli {
    #[arg(value_name = "PROJECT")]
    pub project: Option<PathBuf>,
    #[arg(long, value_name = "URL", value_parser = parse_server_url)]
    pub server: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub backend_bin: Option<PathBuf>,
    #[arg(long, value_name = "PATH", default_value = "bun")]
    pub bun: PathBuf,
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

impl Cli {
    /// Validate argument relationships before any process starts.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.fork && !self.r#continue && self.session.is_none() {
            return Err("--fork requires --continue or --session");
        }
        Ok(())
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

fn parse_server_url(value: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(value).map_err(|error| format!("invalid server URL: {error}"))?;
    match url.scheme() {
        "http" | "https" if url.host().is_some() => Ok(value.to_string()),
        _ => Err("server URL must use http or https and include a host".to_string()),
    }
}
