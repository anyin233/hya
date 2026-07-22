use std::error::Error;
use std::io;
use std::mem::MaybeUninit;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser as _;
use hya_sdk::ServerHandle;
use hya_ts::{
    Cli, Command, backend_auth_args, build_bun_command_from, resolve_backend_bin,
    resolve_runtime_dir,
};
use tokio::process::Command as TokioCommand;

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("hya-ts: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<u8, Box<dyn Error>> {
    let cli = Cli::parse();
    cli.validate()?;

    if let Some(command) = &cli.command {
        return run_auth_command(&cli, command).await;
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
        let handle =
            ServerHandle::spawn_hya_backend(&backend.to_string_lossy(), project_str(&project)?)
                .await?;
        let url = handle.base_url().to_string();
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
    let child = TokioCommand::new(spec.program)
        .args(spec.args)
        .current_dir(spec.current_dir)
        .process_group(0)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            restore_terminal(&mut terminal)?;
            drop(owned);
            return Err(error.into());
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

/// Forward auth/oauth commands to the sibling `hya-backend` binary (same store/config).
async fn run_auth_command(cli: &Cli, command: &Command) -> Result<u8, Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    let backend = resolve_backend_bin(
        cli.backend_bin.as_deref(),
        std::env::var_os("HYA_BACKEND_BIN").as_deref(),
        &executable,
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .as_path(),
    );
    let args = backend_auth_args(command);
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
