//! Process entry for the TypeScript TUI launcher.
//!
//! Parses [`hya_ts::Cli`], runs backend subcommands or import, otherwise starts
//! an owned `hya-backend` (unless `--server`), hands off the terminal process
//! group to Bun, and restores terminal state on exit.

use std::error::Error;
use std::io;
use std::mem::MaybeUninit;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser as _;
use hya_sdk::ServerHandle;
use hya_ts::{
    Cli, Command, backend_command_args, build_bun_command_from, build_bun_command_with_url_fifo,
    invocation_name, resolve_backend_bin, resolve_runtime_dir,
};
use tokio::process::Command as TokioCommand;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{}: {error}", invocation_name());
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<u8, Box<dyn Error>> {
    let cli = Cli::parse();
    cli.validate()?;

    if let Some(command) = &cli.command {
        return run_backend_command(&cli, command).await;
    }
    if let Some(source) = cli.import.as_deref() {
        cmd_import(source)?;
        return Ok(0);
    }

    let cwd = std::env::current_dir()?;
    let project = cli.project.as_deref().unwrap_or(&cwd).canonicalize()?;
    let executable = std::env::current_exe()?;
    let runtime = resolve_runtime_dir(
        std::env::var_os("HYA_TUI_TS_DIR").as_deref(),
        &executable,
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .as_path(),
    )?;

    emit_startup_mark("hya_ts_start", None);
    let mut owned = None;
    let url = if let Some(url) = cli.server.as_deref() {
        url.to_string()
    } else {
        let backend = resolve_backend_bin(
            cli.backend_bin.as_deref(),
            std::env::var_os("HYA_BACKEND_BIN").as_deref(),
            &executable,
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .as_path(),
        );
        emit_startup_mark("backend_spawn", Some(&backend.display().to_string()));
        if startup_overlap_enabled() {
            return run_owned_with_overlap(cli, &runtime, &cwd, &project, &backend).await;
        }
        let handle =
            ServerHandle::spawn_hya_backend(&backend.to_string_lossy(), project_str(&project)?)
                .await?;
        let url = handle.base_url().to_string();
        emit_startup_mark("backend_listen", Some(&url));
        owned = Some(handle);
        url
    };

    let spec = if cli.server.is_some() {
        build_bun_command_from(&cli, &runtime, &cwd)?
    } else {
        let mut attached = cli;
        attached.server = Some(url);
        build_bun_command_from(&attached, &runtime, &cwd)?
    };
    let mut terminal = TerminalState::capture()?;
    let program = spec.program;
    let child = TokioCommand::new(&program)
        .args(spec.args)
        .current_dir(spec.current_dir)
        .process_group(0)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            restore_terminal(&mut terminal)?;
            drop(owned);
            return Err(format!("failed to launch Bun `{}`: {error}", program.display()).into());
        }
    };
    let pid = child.id();

    if let Some(state) = terminal.as_ref() {
        let Some(pgid) = pid.map(|pid| pid as libc::pid_t) else {
            let cleanup = terminate_child_group(&mut child, None).await;
            let restoration = restore_terminal(&mut terminal);
            restoration?;
            cleanup?;
            return Err(io::Error::other("spawned Bun process has no process ID").into());
        };
        if let Err(error) = state.handoff(pgid) {
            let cleanup = terminate_child_group(&mut child, Some(pgid)).await;
            let restoration = restore_terminal(&mut terminal);
            restoration?;
            cleanup?;
            return Err(error.into());
        }
        unsafe {
            libc::kill(-pgid, libc::SIGCONT);
        }
    }

    let result = tokio::select! {
        status = child.wait() => status.map(|status| status.code().and_then(|code| u8::try_from(code).ok()).unwrap_or(1)),
        signal = termination_signal() => {
            match signal {
                Ok(()) => terminate_child_group(&mut child, pid.map(|pid| pid as libc::pid_t)).await.map(|()| 1),
                Err(error) => match terminate_child_group(&mut child, pid.map(|pid| pid as libc::pid_t)).await {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(cleanup),
                },
            }
        }
    };
    restore_terminal(&mut terminal)?;
    drop(owned);
    Ok(result?)
}

fn startup_overlap_enabled() -> bool {
    match std::env::var("HYA_STARTUP_OVERLAP") {
        Ok(value) => {
            let text = value.trim();
            !(text.eq_ignore_ascii_case("0")
                || text.eq_ignore_ascii_case("false")
                || text.eq_ignore_ascii_case("no")
                || text.eq_ignore_ascii_case("off"))
        }
        // Default on: Bun module load overlaps backend listen via FIFO URL handoff.
        Err(_) => true,
    }
}

/// Owned-mode path that overlaps Bun module load with backend listen via a FIFO.
async fn run_owned_with_overlap(
    cli: Cli,
    runtime: &Path,
    cwd: &Path,
    project: &Path,
    backend: &Path,
) -> Result<u8, Box<dyn Error>> {
    let fifo_path = std::env::temp_dir().join(format!("hya-url-{}.fifo", std::process::id()));
    let _ = std::fs::remove_file(&fifo_path);
    nix_mkfifo(&fifo_path)?;
    let mut boot_cli = cli;
    boot_cli.server = Some("http://127.0.0.1:0".into());
    let spec = build_bun_command_with_url_fifo(&boot_cli, runtime, cwd, &fifo_path)?;

    let mut terminal = TerminalState::capture()?;
    let program = spec.program.clone();
    let mut bun_cmd = TokioCommand::new(&program);
    bun_cmd
        .args(&spec.args)
        .current_dir(&spec.current_dir)
        .env("HYA_SERVER_URL_FIFO", &fifo_path)
        .process_group(0);
    let mut child = match bun_cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = std::fs::remove_file(&fifo_path);
            restore_terminal(&mut terminal)?;
            return Err(format!("failed to launch Bun `{}`: {error}", program.display()).into());
        }
    };
    emit_startup_mark("bun_spawn", Some("fifo_overlap"));

    let handle =
        match ServerHandle::spawn_hya_backend(&backend.to_string_lossy(), project_str(project)?)
            .await
        {
            Ok(handle) => handle,
            Err(error) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                let _ = std::fs::remove_file(&fifo_path);
                restore_terminal(&mut terminal)?;
                return Err(error.into());
            }
        };
    let url = handle.base_url().to_string();
    emit_startup_mark("backend_listen", Some(&url));
    let fifo_writer = match write_url_fifo(&fifo_path, &url) {
        Ok(file) => file,
        Err(error) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let _ = std::fs::remove_file(&fifo_path);
            restore_terminal(&mut terminal)?;
            drop(handle);
            return Err(format!("failed to write listen URL to FIFO: {error}").into());
        }
    };

    let pid = child.id();
    if let Some(state) = terminal.as_ref() {
        let Some(pgid) = pid.map(|pid| pid as libc::pid_t) else {
            let cleanup = terminate_child_group(&mut child, None).await;
            let restoration = restore_terminal(&mut terminal);
            drop(fifo_writer);
            let _ = std::fs::remove_file(&fifo_path);
            restoration?;
            cleanup?;
            return Err(io::Error::other("spawned Bun process has no process ID").into());
        };
        if let Err(error) = state.handoff(pgid) {
            let cleanup = terminate_child_group(&mut child, Some(pgid)).await;
            let restoration = restore_terminal(&mut terminal);
            drop(fifo_writer);
            let _ = std::fs::remove_file(&fifo_path);
            restoration?;
            cleanup?;
            return Err(error.into());
        }
        unsafe {
            libc::kill(-pgid, libc::SIGCONT);
        }
    }

    let result = tokio::select! {
        status = child.wait() => status.map(|status| status.code().and_then(|code| u8::try_from(code).ok()).unwrap_or(1)),
        signal = termination_signal() => {
            match signal {
                Ok(()) => terminate_child_group(&mut child, pid.map(|pid| pid as libc::pid_t)).await.map(|()| 1),
                Err(error) => match terminate_child_group(&mut child, pid.map(|pid| pid as libc::pid_t)).await {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(cleanup),
                },
            }
        }
    };
    restore_terminal(&mut terminal)?;
    drop(fifo_writer);
    let _ = std::fs::remove_file(&fifo_path);
    drop(handle);
    Ok(result?)
}

fn nix_mkfifo(path: &Path) -> Result<(), Box<dyn Error>> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    if rc != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn write_url_fifo(path: &Path, url: &str) -> io::Result<std::fs::File> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    // O_RDWR on a FIFO does not block for a peer on Linux.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }
    }
    file.write_all(url.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(file)
}

fn cmd_import(source: &str) -> Result<(), Box<dyn Error>> {
    if !source.eq_ignore_ascii_case("compat") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown import source {source}; currently supported: compat"),
        )
        .into());
    }
    let compat_path = hya_app::config::default_compat_config_path().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "no Compat config found; set COMPAT_CONFIG or create ~/.config/opencode/opencode.json",
        )
    })?;
    let summary = hya_app::config::import_compat_models_into_config(
        &compat_path,
        &hya_app::config::expected_config_path(),
    )?;
    println!(
        "hya: imported {} providers and {} models from Compat into {}",
        summary.providers,
        summary.models,
        summary.config_path.display()
    );
    println!("hya: skills import: TODO");
    println!(
        "hya: imported {} local MCP servers and skipped {} unsupported MCP entries",
        summary.mcp_servers, summary.mcp_skipped
    );
    Ok(())
}

/// Forward backend-owned commands to the sibling `hya-backend` binary.
async fn run_backend_command(cli: &Cli, command: &Command) -> Result<u8, Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    let backend = resolve_backend_bin(
        cli.backend_bin.as_deref(),
        std::env::var_os("HYA_BACKEND_BIN").as_deref(),
        &executable,
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .as_path(),
    );
    let args = backend_command_args(command);
    let status = TokioCommand::new(&backend)
        .args(&args)
        .status()
        .await
        .map_err(|error| {
            format!(
                "failed to run {} {}: {error}",
                backend.display(),
                args.iter()
                    .map(|a| a.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })?;
    Ok(status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1))
}

struct TerminalState {
    previous_foreground: libc::pid_t,
    termios: libc::termios,
    restored: bool,
}

impl TerminalState {
    fn capture() -> io::Result<Option<Self>> {
        let previous_foreground = unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) };
        if previous_foreground == -1 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ENOTTY) {
                Ok(None)
            } else {
                Err(error)
            };
        }

        let mut termios = MaybeUninit::uninit();
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, termios.as_mut_ptr()) } == -1 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ENOTTY) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        let current = unsafe { libc::getpgrp() };
        if current != previous_foreground {
            return Err(io::Error::other(format!(
                "launcher process group {current} is not terminal foreground group {previous_foreground}"
            )));
        }

        Ok(Some(Self {
            previous_foreground,
            termios: unsafe { termios.assume_init() },
            restored: false,
        }))
    }

    fn handoff(&self, pgid: libc::pid_t) -> io::Result<()> {
        set_foreground_process_group(pgid)
    }

    fn restore(&mut self) -> io::Result<()> {
        if self.restored {
            return Ok(());
        }
        let foreground = set_foreground_process_group(self.previous_foreground);
        let termios =
            if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.termios) } == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            };
        if foreground.is_ok() && termios.is_ok() {
            self.restored = true;
        }
        foreground.and(termios)
    }
}

impl Drop for TerminalState {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Emit a structured startup mark when `HYA_STARTUP_TRACE` is truthy.
fn emit_startup_mark(mark: &str, detail: Option<&str>) {
    let enabled = std::env::var_os("HYA_STARTUP_TRACE")
        .map(|value| {
            let text = value.to_string_lossy();
            text.eq_ignore_ascii_case("1") || text.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false);
    if !enabled {
        return;
    }
    let wall_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    match detail {
        Some(detail) => {
            let escaped = detail.replace('\\', "\\\\").replace('"', "\\\"");
            eprintln!(
                r#"{{"hya_startup":true,"mark":"{mark}","wall_ms":{wall_ms},"detail":"{escaped}"}}"#
            );
        }
        None => eprintln!(r#"{{"hya_startup":true,"mark":"{mark}","wall_ms":{wall_ms}}}"#),
    }
}

fn restore_terminal(terminal: &mut Option<TerminalState>) -> io::Result<()> {
    match terminal {
        Some(terminal) => terminal.restore(),
        None => Ok(()),
    }
}

fn set_foreground_process_group(pgid: libc::pid_t) -> io::Result<()> {
    let mut blocked = MaybeUninit::uninit();
    let mut previous = MaybeUninit::uninit();
    unsafe {
        libc::sigemptyset(blocked.as_mut_ptr());
        libc::sigaddset(blocked.as_mut_ptr(), libc::SIGTTOU);
    }
    let blocked = unsafe { blocked.assume_init() };
    let mask_error =
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, previous.as_mut_ptr()) };
    if mask_error != 0 {
        return Err(io::Error::from_raw_os_error(mask_error));
    }
    let previous = unsafe { previous.assume_init() };
    let foreground = if unsafe { libc::tcsetpgrp(libc::STDIN_FILENO, pgid) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    };
    let mask_error =
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) };
    if mask_error != 0 {
        return Err(io::Error::from_raw_os_error(mask_error));
    }
    foreground
}

async fn terminate_child_group(
    child: &mut tokio::process::Child,
    pgid: Option<libc::pid_t>,
) -> io::Result<()> {
    if let Some(pgid) = pgid {
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
            libc::kill(-pgid, libc::SIGCONT);
        }
    } else {
        child.start_kill()?;
    }
    match tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
        Ok(status) => status.map(|_| ()),
        Err(_) => {
            if let Some(pgid) = pgid {
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            } else {
                child.start_kill()?;
            }
            child.wait().await.map(|_| ())
        }
    }
}

async fn termination_signal() -> std::io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        signal = tokio::signal::ctrl_c() => signal,
        _ = terminate.recv() => Ok(()),
    }
}

fn project_str(project: &Path) -> Result<&str, Box<dyn Error>> {
    project
        .to_str()
        .ok_or_else(|| format!("project path is not valid UTF-8: {}", project.display()).into())
}
