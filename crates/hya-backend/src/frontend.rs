//! Bare `hya` on a terminal: the TUI and the WebUI (`docs/cli.md`, "Bare
//! `hya`"; ADR-0020).
//!
//! `hya` only orchestrates processes here; rendering stays in the Bun
//! frontends (ADR-0018/0019):
//!
//! 1. the v1 server runs in this process on `127.0.0.1:<free port>` (the same
//!    composition as `hya serve`, without the readiness line);
//! 2. the web host (`packages/hya-tui-web`) serves the WebUI on
//!    `127.0.0.1:<port>`; every browser tab runs the TUI against that server;
//! 3. the terminal TUI (`packages/hya-tui`) runs on this terminal with
//!    `--web-url <url>` or `--web-error <reason>`.
//!
//! When the terminal TUI exits (or `hya` gets SIGINT/SIGTERM/SIGHUP) the web
//! host is stopped (SIGTERM, SIGKILL after a grace period; it stops its tabs'
//! TUIs itself), the server drains and shuts down, and `hya` exits with the
//! TUI's status. While the TUI owns the terminal, this process's stdin is
//! `/dev/null` and its stdout/stderr (server notices, web host output) go to
//! the log file `<state dir>/hya.log`.

use std::ffi::OsString;
use std::fs::File;
use std::io::Write as _;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::{Child, Command};

use crate::serve;

/// How long the web host may take to print its readiness line.
const WEB_READY_TIMEOUT: Duration = Duration::from_secs(20);
/// Wait after SIGTERM before SIGKILL for the web host (it gives its tabs'
/// TUIs a few seconds itself) and for the terminal TUI.
const STOP_GRACE: Duration = Duration::from_secs(8);
/// The log file is rotated to `hya.log.1` at startup once it is this large.
const LOG_ROTATE_BYTES: u64 = 4 * 1024 * 1024;
/// Most bytes of the web host's stderr kept to explain a failed start.
const STDERR_KEEP: usize = 4096;

/// A Bun package `hya` runs: where to find it and how to name it in errors.
pub(crate) struct Asset {
    label: &'static str,
    env: &'static str,
    installed: &'static str,
    workspace: &'static str,
}

/// The OpenTUI terminal frontend.
pub(crate) const TUI_ASSET: Asset = Asset {
    label: "TUI",
    env: "HYA_TUI_DIR",
    installed: "lib/hya/tui",
    workspace: "packages/hya-tui",
};

/// The PTY-to-browser host that serves the WebUI.
pub(crate) const WEB_ASSET: Asset = Asset {
    label: "WebUI host",
    env: "HYA_TUI_WEB_DIR",
    installed: "lib/hya/tui-web",
    workspace: "packages/hya-tui-web",
};

/// Bare `hya` starts the frontends only when both stdin and stdout are
/// terminals; otherwise it prints the guidance banner.
pub(crate) fn should_launch(stdin_tty: bool, stdout_tty: bool) -> bool {
    stdin_tty && stdout_tty
}

fn has_entry(dir: &Path) -> bool {
    dir.join("src/main.ts").is_file()
}

/// `<exe>/../<installed>`, also through the resolved executable when `exe`
/// is a symlink (for example `~/.local/bin/hya` pointing into a prefix).
fn installed_candidates(asset: &Asset, executable: &Path) -> Vec<PathBuf> {
    // `<prefix>/bin/hya` → `<prefix>/<installed>` (lexically, so it does not
    // depend on `bin/..` resolving).
    let beside = |exe: &Path| {
        exe.parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new(".."))
            .join(asset.installed)
    };
    let mut candidates = vec![beside(executable)];
    if let Ok(real) = executable.canonicalize() {
        let candidate = beside(&real);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

/// Find an asset: the `HYA_TUI_DIR` / `HYA_TUI_WEB_DIR` override, else the
/// installed-adjacent `<exe>/../lib/hya/…`, else the workspace package (the
/// order of `hya_app::plugins`' Bun adapter lookup). The directory must hold
/// `src/main.ts` and installed dependencies (`node_modules/`).
pub(crate) fn resolve_asset_dir(
    asset: &Asset,
    override_dir: Option<PathBuf>,
    executable: &Path,
    workspace_root: &Path,
) -> Result<PathBuf, String> {
    let dir = if let Some(dir) = override_dir {
        if !has_entry(&dir) {
            return Err(format!(
                "{}={} has no src/main.ts (the {})",
                asset.env,
                dir.display(),
                asset.label
            ));
        }
        dir
    } else {
        let installed = installed_candidates(asset, executable);
        if let Some(dir) = installed.iter().find(|dir| has_entry(dir)) {
            dir.canonicalize().unwrap_or_else(|_| dir.clone())
        } else {
            let workspace = workspace_root.join(asset.workspace);
            if !has_entry(&workspace) {
                let searched: Vec<String> = installed
                    .iter()
                    .chain(std::iter::once(&workspace))
                    .map(|dir| dir.display().to_string())
                    .collect();
                return Err(format!(
                    "the {} is not installed: no src/main.ts in {}; reinstall hya or set {}",
                    asset.label,
                    searched.join(", "),
                    asset.env
                ));
            }
            workspace
        }
    };
    if !dir.join("node_modules").is_dir() {
        return Err(format!(
            "the {} in {} has no dependencies: run `bun install --frozen-lockfile` in that directory",
            asset.label,
            dir.display()
        ));
    }
    Ok(dir)
}

/// Bun is required for the TUI and the WebUI. `found` is
/// `hya_app::plugins::find_bun()`: `$BUN` as given, else `bun` on `PATH`.
pub(crate) fn check_bun(found: Option<PathBuf>) -> Result<PathBuf, String> {
    match found {
        Some(path) if path.is_file() => Ok(path),
        Some(path) => Err(format!(
            "Bun not found at {} (the BUN environment variable); bare `hya` runs the TUI and the WebUI with Bun",
            path.display()
        )),
        None => Err(
            "Bun is required for the TUI and the WebUI but was not found on PATH: install it from https://bun.sh or set BUN=<path>. Other subcommands (`hya serve`, `hya exec`, …) do not need it."
                .to_string(),
        ),
    }
}

/// The URL from the web host's `hya-tui-web listening on <url>` line.
pub(crate) fn parse_web_ready_line(line: &str) -> Option<String> {
    let url = line
        .trim()
        .strip_prefix("hya-tui-web listening on ")?
        .split_whitespace()
        .next()?;
    Some(url.to_string())
}

/// A short reason for a web host that exited before it was ready.
pub(crate) fn web_failure_reason(code: Option<i32>, stderr: &str, port: u16) -> String {
    if stderr.contains("in use") {
        return format!("port {port} is in use");
    }
    let Some(code) = code else {
        return "web host exited by a signal".to_string();
    };
    match stderr.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => format!("web host exited with code {code}: {line}"),
        None => format!("web host exited with code {code}"),
    }
}

/// argv of the web host: every browser tab runs the TUI against `backend`.
pub(crate) fn web_host_argv(
    bun: &Path,
    web_dir: &Path,
    tui_dir: &Path,
    port: u16,
    cwd: &Path,
    backend: &str,
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = vec![
        bun.into(),
        web_dir.join("src/main.ts").into(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string().into(),
        "--cwd".into(),
        cwd.into(),
        "--".into(),
    ];
    argv.extend(tui_base_argv(bun, tui_dir, backend, cwd));
    argv
}

fn tui_base_argv(bun: &Path, tui_dir: &Path, backend: &str, cwd: &Path) -> Vec<OsString> {
    vec![
        bun.into(),
        tui_dir.join("src/main.ts").into(),
        "--server".into(),
        backend.into(),
        "--dir".into(),
        cwd.into(),
    ]
}

/// Whether the WebUI came up: its URL, or why not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WebStatus {
    /// The web host is listening on this URL.
    Ready(String),
    /// The web host could not start; the reason is shown in the TUI.
    Failed(String),
}

/// argv of the terminal TUI.
pub(crate) fn tui_argv(
    bun: &Path,
    tui_dir: &Path,
    backend: &str,
    cwd: &Path,
    web: &WebStatus,
) -> Vec<OsString> {
    let mut argv = tui_base_argv(bun, tui_dir, backend, cwd);
    match web {
        WebStatus::Ready(url) => argv.extend(["--web-url".into(), url.into()]),
        WebStatus::Failed(reason) => argv.extend(["--web-error".into(), reason.into()]),
    }
    argv
}

/// `hya`'s exit status for the TUI's: its code, or `128 + signal`.
pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

/// The log file of bare `hya` in the state directory (`$XDG_STATE_HOME/hya`).
pub(crate) fn log_path(state_dir: &Path) -> PathBuf {
    state_dir.join("hya.log")
}

/// What bare `hya` was asked to run.
pub(crate) struct LaunchRequest {
    /// WebUI port (`--port`; 0 = a free port).
    pub(crate) port: u16,
    /// Resolved database path (`resolve_interactive_db`).
    pub(crate) db: String,
    pub(crate) model: Option<String>,
    pub(crate) yolo: bool,
    pub(crate) pure: bool,
    /// `$XDG_STATE_HOME/hya` (created), home of the log file.
    pub(crate) state_dir: PathBuf,
}

/// The paths bare `hya` runs: Bun, the two packages, and the workspace.
struct Resolved {
    bun: PathBuf,
    tui: PathBuf,
    web: PathBuf,
    cwd: PathBuf,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .unwrap_or(manifest)
        .to_path_buf()
}

fn resolve() -> anyhow::Result<Resolved> {
    let bun = check_bun(hya_app::plugins::find_bun()).map_err(anyhow::Error::msg)?;
    let executable = std::env::current_exe().unwrap_or_default();
    let root = workspace_root();
    let tui = resolve_asset_dir(&TUI_ASSET, env_path(TUI_ASSET.env), &executable, &root)
        .map_err(anyhow::Error::msg)?;
    let web = resolve_asset_dir(&WEB_ASSET, env_path(WEB_ASSET.env), &executable, &root)
        .map_err(anyhow::Error::msg)?;
    let cwd = std::env::current_dir().context("read the working directory")?;
    Ok(Resolved { bun, tui, web, cwd })
}

/// Run bare `hya` on a terminal. Returns only on an error before or while
/// starting; otherwise it exits the process with the TUI's status.
pub(crate) async fn run(request: LaunchRequest) -> anyhow::Result<()> {
    // Everything that can fail cheaply fails here, before the terminal is touched.
    let resolved = resolve()?;
    let log_file = log_path(&request.state_dir);
    let log =
        open_log(&log_file).with_context(|| format!("open the log file {}", log_file.display()))?;
    let mut signals = StopSignals::install().context("install signal handlers")?;
    let terminal = Terminal::detach(&log).context("hand the terminal to the TUI")?;
    let outcome = orchestrate(&request, &resolved, &terminal, &mut signals).await;
    terminal.restore();
    match outcome {
        Ok(code) => {
            let _ = std::io::stdout().flush();
            std::process::exit(code);
        }
        Err(error) => {
            eprintln!(
                "hya: see {} for the server and WebUI log",
                log_file.display()
            );
            Err(error)
        }
    }
}

async fn orchestrate(
    request: &LaunchRequest,
    resolved: &Resolved,
    terminal: &Terminal,
    signals: &mut StopSignals,
) -> anyhow::Result<i32> {
    eprintln!(
        "hya {}: bare launch pid {} in {} (db {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        resolved.cwd.display(),
        request.db
    );
    let prepared = tokio::select! {
        prepared = serve::prepare_server(
            "127.0.0.1:0",
            request.db.clone(),
            request.model.clone(),
            request.yolo,
            request.pure,
        ) => prepared.context("start the server")?,
        signal = signals.recv() => return Ok(128 + signal),
    };
    let backend = prepared.url.clone();
    eprintln!("hya: server listening on {backend}");
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = serve::serve_until(prepared, async move {
        let _ = stop_rx.await;
    });
    let frontends = async move {
        let code = run_frontends(request.port, resolved, &backend, terminal, signals).await;
        let _ = stop_tx.send(());
        code
    };
    let (served, code) = tokio::join!(server, frontends);
    if let Err(error) = served {
        eprintln!("hya: server shutdown: {error:#}");
    }
    code
}

async fn run_frontends(
    port: u16,
    resolved: &Resolved,
    backend: &str,
    terminal: &Terminal,
    signals: &mut StopSignals,
) -> anyhow::Result<i32> {
    let Resolved { bun, tui, web, cwd } = resolved;
    let host_argv = web_host_argv(bun, web, tui, port, cwd, backend);
    let started = tokio::select! {
        started = start_web_host(&host_argv, cwd, port) => started,
        signal = signals.recv() => return Ok(128 + signal),
    };
    let (host, web_status) = match started {
        Ok((child, url)) => (Some(child), WebStatus::Ready(url)),
        Err(reason) => (None, WebStatus::Failed(reason)),
    };
    eprintln!("hya: WebUI {web_status:?}");
    let argv = tui_argv(bun, tui, backend, cwd, &web_status);
    let code = match spawn_tui(&argv, cwd, terminal) {
        Ok(mut child) => {
            tokio::select! {
                status = child.wait() => status.map(exit_code).context("wait for the TUI"),
                signal = signals.recv() => {
                    eprintln!("hya: signal {signal}; stopping the TUI");
                    stop_child(&mut child, "TUI").await;
                    Ok(128 + signal)
                }
            }
        }
        Err(error) => Err(error),
    };
    if let Some(mut host) = host {
        stop_child(&mut host, "web host").await;
    }
    eprintln!("hya: frontends stopped ({code:?})");
    code
}

/// Start the web host and wait for its readiness line; on failure the reason
/// the TUI shows (`WebUI unavailable: <reason>`).
async fn start_web_host(
    argv: &[OsString],
    cwd: &Path,
    port: u16,
) -> Result<(Child, String), String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("empty web host command".to_string());
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own process group: a terminal Ctrl+C or hangup reaches the TUI
        // and `hya`, and `hya` stops the host in order.
        .process_group(0)
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start the web host: {error}"))?;
    let stderr_text = Arc::new(Mutex::new(String::new()));
    if let Some(stderr) = child.stderr.take() {
        let keep = Arc::clone(&stderr_text);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("[webui] {line}");
                if let Ok(mut text) = keep.lock()
                    && text.len() < STDERR_KEEP
                {
                    text.push_str(&line);
                    text.push('\n');
                }
            }
        });
    }
    let Some(stdout) = child.stdout.take() else {
        return Err("web host stdout unavailable".to_string());
    };
    let mut lines = BufReader::new(stdout).lines();
    let ready = tokio::time::timeout(WEB_READY_TIMEOUT, async {
        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[webui] {line}");
            if let Some(url) = parse_web_ready_line(&line) {
                return Some(url);
            }
        }
        None
    })
    .await;
    match ready {
        Ok(Some(url)) => {
            tokio::spawn(async move {
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!("[webui] {line}");
                }
            });
            Ok((child, url))
        }
        Ok(None) => {
            let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            // Let the stderr reader take the last lines.
            tokio::time::sleep(Duration::from_millis(50)).await;
            let code = match status {
                Ok(Ok(status)) => status.code(),
                _ => None,
            };
            let text = stderr_text
                .lock()
                .map(|text| text.clone())
                .unwrap_or_default();
            Err(web_failure_reason(code, &text, port))
        }
        Err(_) => {
            stop_child(&mut child, "web host").await;
            Err(format!(
                "the web host printed no readiness line within {} s",
                WEB_READY_TIMEOUT.as_secs()
            ))
        }
    }
}

fn spawn_tui(argv: &[OsString], cwd: &Path, terminal: &Terminal) -> anyhow::Result<Child> {
    let Some((program, args)) = argv.split_first() else {
        anyhow::bail!("empty TUI command");
    };
    let (stdin, stdout, stderr) = terminal.stdio().context("pass the terminal to the TUI")?;
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("start the TUI ({})", Path::new(program).display()))
}

/// SIGTERM, then SIGKILL after [`STOP_GRACE`]; waits for the exit.
async fn stop_child(child: &mut Child, name: &str) {
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }
    if let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        // SAFETY: `kill` has no memory-safety preconditions; `pid` is our
        // own child, not yet reaped (`try_wait` above), so it is not reused.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    match tokio::time::timeout(STOP_GRACE, child.wait()).await {
        Ok(status) => eprintln!("hya: {name} exited ({status:?})"),
        Err(_) => {
            eprintln!("hya: {name} ignored SIGTERM; killing it");
            let _ = child.kill().await;
        }
    }
}

/// SIGINT, SIGTERM, and SIGHUP, installed before any child starts.
struct StopSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

impl StopSignals {
    fn install() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    /// The next stop signal's number.
    async fn recv(&mut self) -> i32 {
        tokio::select! {
            _ = self.interrupt.recv() => libc::SIGINT,
            _ = self.terminate.recv() => libc::SIGTERM,
            _ = self.hangup.recv() => libc::SIGHUP,
        }
    }
}

/// Append-only log; rotated once to `hya.log.1` when it grows too large.
fn open_log(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() > LOG_ROTATE_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// The terminal's stdin/stdout/stderr, saved while this process's own fds
/// point at `/dev/null` and the log, so only the TUI draws on the terminal
/// and only the TUI reads it (the server's children inherit the log, not the
/// terminal).
struct Terminal {
    stdin: OwnedFd,
    stdout: OwnedFd,
    stderr: OwnedFd,
}

impl Terminal {
    fn detach(log: &File) -> std::io::Result<Self> {
        let saved = Self {
            stdin: std::io::stdin().as_fd().try_clone_to_owned()?,
            stdout: std::io::stdout().as_fd().try_clone_to_owned()?,
            stderr: std::io::stderr().as_fd().try_clone_to_owned()?,
        };
        let null = File::open("/dev/null")?;
        let _ = std::io::stdout().flush();
        redirect(libc::STDIN_FILENO, null.as_raw_fd())?;
        redirect(libc::STDOUT_FILENO, log.as_raw_fd())?;
        redirect(libc::STDERR_FILENO, log.as_raw_fd())?;
        Ok(saved)
    }

    /// Point stdin/stdout/stderr back at the terminal.
    fn restore(&self) {
        let _ = std::io::stdout().flush();
        let _ = redirect(libc::STDIN_FILENO, self.stdin.as_raw_fd());
        let _ = redirect(libc::STDOUT_FILENO, self.stdout.as_raw_fd());
        let _ = redirect(libc::STDERR_FILENO, self.stderr.as_raw_fd());
    }

    fn stdio(&self) -> std::io::Result<(Stdio, Stdio, Stdio)> {
        Ok((
            Stdio::from(self.stdin.try_clone()?),
            Stdio::from(self.stdout.try_clone()?),
            Stdio::from(self.stderr.try_clone()?),
        ))
    }
}

/// `dup2(source, target)`.
fn redirect(target: RawFd, source: RawFd) -> std::io::Result<()> {
    // SAFETY: `dup2` only manipulates the descriptor table; both descriptors
    // are open (owned by the caller or the standard streams), and replacing
    // a standard stream is the intended effect.
    if unsafe { libc::dup2(source, target) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hya-frontend-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        /// A package directory with `src/main.ts` and `node_modules/`.
        fn package(&self, relative: &str) -> PathBuf {
            let dir = self.0.join(relative);
            std::fs::create_dir_all(dir.join("src")).unwrap();
            std::fs::create_dir_all(dir.join("node_modules")).unwrap();
            std::fs::write(dir.join("src/main.ts"), "").unwrap();
            dir
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn os(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn launches_only_when_stdin_and_stdout_are_terminals() {
        assert!(should_launch(true, true));
        assert!(!should_launch(false, true));
        assert!(!should_launch(true, false));
        assert!(!should_launch(false, false));
    }

    #[test]
    fn asset_override_wins_and_must_contain_the_entry() {
        let scratch = Scratch::new("override");
        let custom = scratch.package("custom-tui");
        let installed = scratch.package("prefix/lib/hya/tui");
        let exe = scratch.0.join("prefix/bin/hya");
        let found = resolve_asset_dir(&TUI_ASSET, Some(custom.clone()), &exe, &scratch.0).unwrap();
        assert_eq!(found, custom);
        assert!(installed.is_dir());

        let empty = scratch.0.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let error =
            resolve_asset_dir(&TUI_ASSET, Some(empty.clone()), &exe, &scratch.0).unwrap_err();
        assert!(error.contains("HYA_TUI_DIR"), "{error}");
        assert!(error.contains(&empty.display().to_string()), "{error}");
    }

    #[test]
    fn asset_resolution_prefers_installed_then_workspace() {
        let scratch = Scratch::new("order");
        let exe = scratch.0.join("prefix/bin/hya");
        let workspace = scratch.0.join("repo");
        let workspace_web = scratch.package("repo/packages/hya-tui-web");
        assert_eq!(
            resolve_asset_dir(&WEB_ASSET, None, &exe, &workspace).unwrap(),
            workspace_web
        );
        let installed_web = scratch.package("prefix/lib/hya/tui-web");
        let found = resolve_asset_dir(&WEB_ASSET, None, &exe, &workspace).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            installed_web.canonicalize().unwrap()
        );
    }

    #[test]
    fn missing_assets_name_every_place_searched() {
        let scratch = Scratch::new("missing");
        let exe = scratch.0.join("prefix/bin/hya");
        let error = resolve_asset_dir(&TUI_ASSET, None, &exe, &scratch.0.join("repo")).unwrap_err();
        assert!(error.contains("lib/hya/tui"), "{error}");
        assert!(error.contains("packages/hya-tui"), "{error}");
        assert!(error.contains("HYA_TUI_DIR"), "{error}");
    }

    #[test]
    fn assets_without_dependencies_ask_for_bun_install() {
        let scratch = Scratch::new("deps");
        let dir = scratch.0.join("repo/packages/hya-tui");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.ts"), "").unwrap();
        let exe = scratch.0.join("prefix/bin/hya");
        let error = resolve_asset_dir(&TUI_ASSET, None, &exe, &scratch.0.join("repo")).unwrap_err();
        assert!(error.contains("bun install"), "{error}");
    }

    #[test]
    fn bun_must_exist() {
        let scratch = Scratch::new("bun");
        let bun = scratch.0.join("bun");
        std::fs::write(&bun, "").unwrap();
        assert_eq!(check_bun(Some(bun.clone())).unwrap(), bun);
        let error = check_bun(Some(scratch.0.join("no-bun"))).unwrap_err();
        assert!(error.contains("no-bun"), "{error}");
        let error = check_bun(None).unwrap_err();
        assert!(error.contains("Bun"), "{error}");
        assert!(error.contains("https://bun.sh"), "{error}");
    }

    #[test]
    fn parses_the_web_host_readiness_line() {
        assert_eq!(
            parse_web_ready_line("hya-tui-web listening on http://127.0.0.1:3250/").as_deref(),
            Some("http://127.0.0.1:3250/")
        );
        assert_eq!(
            parse_web_ready_line("  hya-tui-web listening on http://127.0.0.1:1/\r").as_deref(),
            Some("http://127.0.0.1:1/")
        );
        assert_eq!(
            parse_web_ready_line("hya server listening on http://x"),
            None
        );
        assert_eq!(parse_web_ready_line("hya-tui-web listening on "), None);
    }

    #[test]
    fn explains_why_the_web_host_failed() {
        let busy = "Failed to start server. Is port 3250 in use?\nUsage: bun ...\n";
        assert_eq!(
            web_failure_reason(Some(2), busy, 3250),
            "port 3250 is in use"
        );
        assert_eq!(
            web_failure_reason(Some(1), "\nerror: boom\nmore\n", 3250),
            "web host exited with code 1: error: boom"
        );
        assert_eq!(
            web_failure_reason(None, "", 3250),
            "web host exited by a signal"
        );
    }

    #[test]
    fn builds_the_web_host_command() {
        let argv = web_host_argv(
            Path::new("/b/bun"),
            Path::new("/lib/tui-web"),
            Path::new("/lib/tui"),
            3250,
            Path::new("/work"),
            "http://127.0.0.1:5555",
        );
        assert_eq!(
            argv,
            os(&[
                "/b/bun",
                "/lib/tui-web/src/main.ts",
                "--host",
                "127.0.0.1",
                "--port",
                "3250",
                "--cwd",
                "/work",
                "--",
                "/b/bun",
                "/lib/tui/src/main.ts",
                "--server",
                "http://127.0.0.1:5555",
                "--dir",
                "/work",
            ])
        );
    }

    #[test]
    fn builds_the_terminal_tui_command() {
        let base = [
            "/b/bun",
            "/lib/tui/src/main.ts",
            "--server",
            "http://127.0.0.1:5555",
            "--dir",
            "/work",
        ];
        let ready = tui_argv(
            Path::new("/b/bun"),
            Path::new("/lib/tui"),
            "http://127.0.0.1:5555",
            Path::new("/work"),
            &WebStatus::Ready("http://127.0.0.1:3250/".into()),
        );
        let mut expected = os(&base);
        expected.extend(os(&["--web-url", "http://127.0.0.1:3250/"]));
        assert_eq!(ready, expected);
        let failed = tui_argv(
            Path::new("/b/bun"),
            Path::new("/lib/tui"),
            "http://127.0.0.1:5555",
            Path::new("/work"),
            &WebStatus::Failed("port 3250 is in use".into()),
        );
        let mut expected = os(&base);
        expected.extend(os(&["--web-error", "port 3250 is in use"]));
        assert_eq!(failed, expected);
    }

    #[test]
    fn exit_code_follows_the_tui() {
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(exit_code(std::process::ExitStatus::from_raw(0)), 0);
        assert_eq!(exit_code(std::process::ExitStatus::from_raw(3 << 8)), 3);
        // Killed by SIGTERM (15): 128 + 15.
        assert_eq!(exit_code(std::process::ExitStatus::from_raw(15)), 143);
    }

    #[test]
    fn log_file_lives_under_the_state_directory() {
        assert_eq!(
            log_path(Path::new("/state/hya")),
            PathBuf::from("/state/hya/hya.log")
        );
    }
}
