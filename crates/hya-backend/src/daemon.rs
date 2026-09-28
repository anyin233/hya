//! The persistent backend daemon (ADR-0023; docs/cli.md "Backend daemon").
//!
//! The backend outlives its clients. A client (the TUI, bare `hya`, or
//! `hya serve start`) looks for the running server of a database through its
//! discovery file and a health probe (ADR-0022); when none answers it starts
//! `hya serve --bind 127.0.0.1:0 --db <db>` **detached**: its own session
//! (`setsid`), stdin `/dev/null`, stdout and stderr appended to
//! `<db>.server.log`. Nothing stops the daemon when the client exits; `hya
//! serve stop` (SIGTERM, wait until the database lock is released) does. It
//! first leaves the reason (`stop`/`restart`) in `<db>.server.stop`, which
//! the daemon sends to its clients as the last stream frame: they start the
//! next server only after an unexpected loss.
//!
//! Starting is race-safe: the database lock arbitrates. A starter whose
//! daemon lost the race (it exits 75) waits for the winner's discovery file;
//! while another process holds the lock without answering (it is starting,
//! or shutting down), the starter waits and starts a new daemon as soon as
//! the lock is free.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::os::unix::io::RawFd;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;

use crate::cli_args::RelayFlags;
use crate::db_lock::{self, Discovery};

/// Longest wait for a started daemon to publish a healthy server (a first
/// run may build catalogs).
pub(crate) const START_WAIT: Duration = Duration::from_secs(60);
/// One health probe of a discovered server.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Poll interval while waiting for a server to appear or go away.
const POLL: Duration = Duration::from_millis(100);
/// The daemon log is rotated to `<log>.1` before a start once this large.
const LOG_ROTATE_BYTES: u64 = 4 * 1024 * 1024;
/// Wait after SIGKILL (`stop --force`) for the kernel to release the lock.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// How to start a daemon for one database.
#[derive(Clone, Debug)]
pub(crate) struct DaemonSpec {
    /// The database (a file path; in-memory stores cannot be shared).
    pub(crate) db: String,
    /// `--model`, `--yolo`, `--pure` of the command that starts it; they
    /// shape that daemon for every client until it stops.
    pub(crate) model: Option<String>,
    pub(crate) yolo: bool,
    pub(crate) pure: bool,
    /// `--allow-host` names the daemon accepts besides loopback (Host
    /// allowlist; docs/cli.md "Allowed Host names").
    pub(crate) allow_hosts: Vec<String>,
    /// The `hya` binary to run (`std::env::current_exe()`). For a handoff
    /// successor this is the journal's recorded executable — the invoking
    /// restart CLI's `current_exe`, validated before the spawn.
    pub(crate) exe: PathBuf,
}

/// A running server of a database, and whether this call started it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ready {
    pub(crate) discovery: Discovery,
    pub(crate) started: bool,
}

/// The daemon log of `db` (`<db>.server.log`).
pub(crate) fn log_path(db: &str) -> Option<PathBuf> {
    db_lock::paths(db).map(|paths| paths.log)
}

/// Whether a process with this pid exists (signal 0). Shared with the
/// handoff watchers (the successor watches its predecessor's pid).
pub(crate) fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let status = unsafe { libc::kill(pid, 0) };
    status == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// The live server of `db`: a discovery file whose pid is alive and whose
/// URL answers `GET /v1/health` with `ok: true`.
pub(crate) async fn running(db: &str) -> Option<Discovery> {
    let paths = db_lock::paths(db)?;
    let found = db_lock::read_discovery(&paths.discovery)?;
    if !process_alive(found.pid) {
        return None;
    }
    db_lock::probe(&found.url, PROBE_TIMEOUT)
        .await
        .then_some(found)
}

/// Attach to the running server of `spec.db`, else start a daemon and wait
/// (up to `wait`) until it answers.
pub(crate) async fn start(spec: &DaemonSpec, wait: Duration) -> anyhow::Result<Ready> {
    start_with_relay(spec, &RelayFlags::default(), wait).await
}

/// [`start`], and a daemon it starts joins the relay of `relay` (absolute
/// paths; `hya serve start|restart --relay`). An already running server is
/// left as it is.
pub(crate) async fn start_with_relay(
    spec: &DaemonSpec,
    relay: &RelayFlags,
    wait: Duration,
) -> anyhow::Result<Ready> {
    let Some(paths) = db_lock::paths(&spec.db) else {
        anyhow::bail!(
            "the backend daemon needs a database file; {:?} is in memory (pass --db <path>)",
            spec.db
        );
    };
    if let Some(dir) = paths.lock.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create the database directory {}", dir.display()))?;
    }
    let deadline = tokio::time::Instant::now() + wait;
    let mut child: Option<Child> = None;
    loop {
        if let Some(discovery) = running(&spec.db).await {
            let started = child
                .as_ref()
                .is_some_and(|child| child.id() == discovery.pid);
            if let Some(child) = child.take() {
                reap(child);
            }
            return Ok(Ready { discovery, started });
        }
        if let Some(mut ours) = child.take() {
            match ours.try_wait().context("check the started daemon")? {
                None => child = Some(ours),
                Some(status) if status.code() == Some(db_lock::EXIT_DB_IN_USE) => {
                    // Lost the race, or the holder was still shutting down:
                    // wait for its server, or for the lock to come free.
                }
                Some(status) => anyhow::bail!(
                    "hya serve (daemon) exited with {status} before it was ready; see {}{}",
                    paths.log.display(),
                    log_tail(&paths.log)
                ),
            }
        }
        if child.is_none()
            && db_lock::holder(&spec.db)
                .context("check the database lock")?
                .is_none()
        {
            child = Some(spawn(spec, relay, &paths.log)?);
        }
        if tokio::time::Instant::now() >= deadline {
            let holder = db_lock::holder(&spec.db).ok().flatten();
            if let Some(mut ours) = child.take() {
                let _ = ours.kill();
                let _ = ours.wait();
            }
            anyhow::bail!(match holder {
                Some(busy) => format!(
                    "database {} is held by pid {}, which serves no reachable server (waited {} s); stop it (`hya serve stop --force --db {}`) or pass another --db",
                    spec.db,
                    busy.holder_pid
                        .map_or_else(|| "unknown".to_string(), |pid| pid.to_string()),
                    wait.as_secs(),
                    spec.db
                ),
                None => format!(
                    "the hya server daemon did not answer within {} s; see {}{}",
                    wait.as_secs(),
                    paths.log.display(),
                    log_tail(&paths.log)
                ),
            });
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Wait for a started child in the background, so it never lingers as a
/// zombie of a long-lived starter (bare `hya`).
fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// The daemon's working directory: `$HOME` when it is an existing
/// directory, else `/`. The backend serves every client of the database,
/// wherever each one runs, so it never works in the starter's directory
/// (ADR-0024: requests and sessions carry their own directory; the server has
/// no working directory of its own).
fn daemon_dir(home: Option<std::ffi::OsString>) -> PathBuf {
    home.filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .filter(|home| home.is_dir())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `hya serve --bind 127.0.0.1:0 --db <db>` (plus the spec's `--model`,
/// `--yolo`, `--pure`, and relay flags) run in `dir`; stdio and the session
/// are the caller's.
fn serve_command(spec: &DaemonSpec, relay: &RelayFlags, dir: &std::path::Path) -> Command {
    let mut command = Command::new(&spec.exe);
    command
        .args(["serve", "--bind", "127.0.0.1:0", "--db", &spec.db])
        .current_dir(dir);
    if let Some(model) = &spec.model {
        command.args(["--model", model]);
    }
    if spec.yolo {
        command.arg("--yolo");
    }
    if spec.pure {
        command.arg("--pure");
    }
    for host in &spec.allow_hosts {
        command.args(["--allow-host", host]);
    }
    // The daemon never inherits a relay link or a bridge token of the
    // command that starts it (`hya --connect`, a TUI).
    command
        .env_remove(crate::bridge::LINK_ENV)
        .env_remove(crate::bridge::TOKEN_ENV);
    if let Some(url) = &relay.relay {
        command.args(["--relay", url]);
        if let Some(transport) = &relay.relay_transport {
            command.args(["--relay-transport", transport]);
        }
        if let Some(ca) = &relay.relay_ca {
            command.arg("--relay-ca").arg(ca);
        }
        if relay.relay_ephemeral {
            command.arg("--relay-ephemeral");
        }
        // The daemon's output is its log file: the link must not land there.
        command.arg("--relay-quiet-link");
    }
    if let Some(seconds) = relay.relay_heartbeat {
        command.args(["--relay-heartbeat", &seconds.to_string()]);
    }
    command
}

/// [`serve_command`] in [`daemon_dir`], in its own session, output appended
/// to `log`.
fn spawn(spec: &DaemonSpec, relay: &RelayFlags, log: &std::path::Path) -> anyhow::Result<Child> {
    if std::fs::metadata(log).is_ok_and(|meta| meta.len() > LOG_ROTATE_BYTES) {
        let mut rotated = log.as_os_str().to_owned();
        rotated.push(".1");
        let _ = std::fs::rename(log, rotated);
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("open the daemon log {}", log.display()))?;
    let _ = writeln!(
        file,
        "--- hya {} daemon start by pid {} at {} ms (db {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        unix_ms(),
        spec.db
    );
    let mut command = serve_command(spec, relay, &daemon_dir(std::env::var_os("HOME")));
    command
        .stdin(Stdio::null())
        .stdout(file.try_clone().context("share the daemon log")?)
        .stderr(file);
    // SAFETY: `setsid` is async-signal-safe and touches no memory of the
    // parent; it detaches the daemon from the starter's session and
    // controlling terminal, so neither a terminal hangup nor the starter's
    // exit reaches it.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
        .spawn()
        .with_context(|| format!("start {} serve", spec.exe.display()))
}

/// The staged descriptors and inherited state the old generation hands to
/// its successor on the command line: the listening socket, the held
/// `<db>.lock` flock, the optional extra gRPC listener, the handoff journal,
/// and the predecessor's status timestamp. Every descriptor is a staged
/// duplicate that keeps close-on-exec in the old generation (see
/// `db_lock::DbLock::duplicate_for_handoff` and serve.rs's
/// `stage_for_handoff`);
/// [`spawn_handoff`] clears the flag in the successor's `pre_exec`, so the
/// numbers are inheritable only across that one exec — they never leak into
/// other children of the daemon (tools, shells, bundles).
pub(crate) struct SuccessorSpawn {
    pub(crate) journal: PathBuf,
    pub(crate) listener_fd: RawFd,
    pub(crate) lock_fd: RawFd,
    pub(crate) extra_fd: Option<RawFd>,
    /// `--inherit-status`: the old generation's discovery `startedAt` (unix
    /// ms); the successor republishes it so status and uptime survive.
    pub(crate) started_at: Option<u64>,
    /// A rollback's pinned first-party source root
    /// ([`hya_bundle::FIRST_PARTY_SOURCE_ROOT_ENV`]); `None` for an ordinary
    /// successor, which never inherits one.
    pub(crate) first_party_root: Option<PathBuf>,
}

/// The successor executable recorded in the handoff journal must exist, be a
/// regular file, and be executable. A missing or unusable one fails the
/// handoff with this error — never a fallback to the old generation's own
/// (possibly outdated) binary.
pub(crate) fn validate_successor_exe(exe: &std::path::Path) -> anyhow::Result<()> {
    let meta = std::fs::metadata(exe)
        .with_context(|| format!("the successor executable {} is missing", exe.display()))?;
    anyhow::ensure!(
        meta.is_file(),
        "the successor executable {} is not a regular file",
        exe.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        anyhow::ensure!(
            meta.permissions().mode() & 0o111 != 0,
            "the successor executable {} is not executable",
            exe.display()
        );
    }
    Ok(())
}

/// The journal's relay record of resolved restart flags (public values only;
/// never the link or any key).
pub(crate) fn relay_spec(flags: &RelayFlags) -> Option<db_lock::DiscoveredRelay> {
    let relay = flags.relay.as_ref()?;
    Some(db_lock::DiscoveredRelay {
        proxy_url: relay.clone(),
        transport: flags
            .relay_transport
            .clone()
            .unwrap_or_else(|| "auto".to_owned()),
        ca: flags.relay_ca.clone(),
        ephemeral: flags.relay_ephemeral,
        heartbeat_secs: flags.relay_heartbeat,
    })
}

/// The relay flags a successor joins with, from the journal's record.
pub(crate) fn relay_flags_of(record: &db_lock::DiscoveredRelay) -> RelayFlags {
    RelayFlags {
        relay: Some(record.proxy_url.clone()),
        relay_transport: Some(record.transport.clone()),
        relay_ca: record.ca.clone(),
        relay_ephemeral: record.ephemeral,
        relay_heartbeat: record.heartbeat_secs,
        relay_quiet_link: false,
    }
}

/// The successor command for one handoff: the old generation's own
/// composition inputs (`spec`; `exe` is the journal's validated successor
/// executable), the journal's successor overrides (`model`, `allow_hosts`,
/// `relay`), and the staged descriptors as hidden flags.
fn successor_command(
    spec: &DaemonSpec,
    relay: &RelayFlags,
    handoff: &SuccessorSpawn,
    dir: &std::path::Path,
) -> Command {
    let mut command = Command::new(&spec.exe);
    command
        .args([
            "serve",
            "--listen-fd",
            &handoff.listener_fd.to_string(),
            "--lock-fd",
            &handoff.lock_fd.to_string(),
            "--handoff-journal",
            &handoff.journal.to_string_lossy(),
        ])
        .current_dir(dir);
    if let Some(fd) = handoff.extra_fd {
        command.args(["--grpc-listen-fd", &fd.to_string()]);
    }
    if let Some(ms) = handoff.started_at {
        command.args(["--inherit-status", &ms.to_string()]);
    }
    command.args(["--db", &spec.db]);
    if let Some(model) = &spec.model {
        command.args(["--model", model]);
    }
    if spec.yolo {
        command.arg("--yolo");
    }
    if spec.pure {
        command.arg("--pure");
    }
    for host in &spec.allow_hosts {
        command.args(["--allow-host", host]);
    }
    // The daemon never inherits a relay link or a bridge token of the
    // generation that spawns it, nor a rolled-back generation's pinned
    // first-party root: only a rollback successor gets its own.
    command
        .env_remove(crate::bridge::LINK_ENV)
        .env_remove(crate::bridge::TOKEN_ENV)
        .env_remove(hya_bundle::FIRST_PARTY_SOURCE_ROOT_ENV);
    if let Some(root) = &handoff.first_party_root {
        command.env(hya_bundle::FIRST_PARTY_SOURCE_ROOT_ENV, root);
    }
    if let Some(url) = &relay.relay {
        command.args(["--relay", url]);
        if let Some(transport) = &relay.relay_transport {
            command.args(["--relay-transport", transport]);
        }
        if let Some(ca) = &relay.relay_ca {
            command.arg("--relay-ca").arg(ca);
        }
        if relay.relay_ephemeral {
            command.arg("--relay-ephemeral");
        }
        // The daemon's output is its log file: the link must not land there.
        command.arg("--relay-quiet-link");
    }
    if let Some(seconds) = relay.relay_heartbeat {
        command.args(["--relay-heartbeat", &seconds.to_string()]);
    }
    command
}

/// Spawn the successor with the listening socket, the backend flock, and the
/// optional extra gRPC listener inherited. The descriptors are staged
/// duplicates (close-on-exec set in this process); the `pre_exec` below
/// clears close-on-exec on exactly the handed numbers inside the forked
/// child, so they survive this one exec and nothing else ever sees them.
/// The staged duplicates stay open in the old generation until it finishes
/// the takeover — the successor's health wait and any park keep the flock
/// and the listening socket alive even if the successor dies. The successor
/// runs detached like any daemon ([`spawn`]), its output in the same log.
pub(crate) fn spawn_handoff(
    spec: &DaemonSpec,
    relay: &RelayFlags,
    handoff: &SuccessorSpawn,
    log: &std::path::Path,
) -> anyhow::Result<Child> {
    if std::fs::metadata(log).is_ok_and(|meta| meta.len() > LOG_ROTATE_BYTES) {
        let mut rotated = log.as_os_str().to_owned();
        rotated.push(".1");
        let _ = std::fs::rename(log, rotated);
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("open the daemon log {}", log.display()))?;
    let _ = writeln!(
        file,
        "--- hya {} successor start by pid {} at {} ms (db {})",
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        unix_ms(),
        spec.db
    );
    let mut command =
        successor_command(spec, relay, handoff, &daemon_dir(std::env::var_os("HOME")));
    command
        .stdin(Stdio::null())
        .stdout(file.try_clone().context("share the daemon log")?)
        .stderr(file);
    let mut staged = vec![handoff.listener_fd, handoff.lock_fd];
    staged.extend(handoff.extra_fd);
    // SAFETY: between `fork` and `exec` only async-signal-safe syscalls run:
    // `setsid` detaches the successor from this (dying) generation's session,
    // and `fcntl` clears close-on-exec on exactly the staged descriptors, the
    // single moment they are inheritable. A failure here fails the spawn; the
    // descriptors in this process keep their flags (the copy is per child).
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            for fd in staged.iter().copied() {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command
        .spawn()
        .with_context(|| format!("start successor {}", spec.exe.display()))
}
/// The last lines of the daemon log, for an error message.
fn log_tail(log: &std::path::Path) -> String {
    let Ok(text) = std::fs::read_to_string(log) else {
        return String::new();
    };
    let lines: Vec<&str> = text.lines().rev().take(12).collect();
    if lines.is_empty() {
        return String::new();
    }
    let tail: Vec<&str> = lines.into_iter().rev().collect();
    format!(
        "\n--- {} (last lines) ---\n{}",
        log.display(),
        tail.join("\n")
    )
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// What `stop` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stopped {
    /// No process held the database.
    NotRunning,
    /// The holder (this pid) exited and released the database.
    Stopped { pid: u32, killed: bool },
}

/// Stop the server that holds `db`: leave `reason` for it
/// ([`db_lock::request_stop`]: its clients learn whether to start the next
/// server), SIGTERM, then wait until the lock is free. After `timeout`,
/// `force` sends SIGKILL; otherwise it is an error.
pub(crate) async fn stop(
    db: &str,
    timeout: Duration,
    force: bool,
    reason: hya_server::ShutdownReason,
) -> anyhow::Result<Stopped> {
    let Some(busy) = db_lock::holder(db).context("check the database lock")? else {
        return Ok(Stopped::NotRunning);
    };
    let Some(pid) = busy
        .holder_pid
        .or_else(|| busy.discovery.as_ref().map(|found| found.pid))
    else {
        anyhow::bail!(
            "database {db} is locked ({}) by a process whose pid is unknown; stop it by hand",
            busy.paths.lock.display()
        );
    };
    let raw = i32::try_from(pid).context("pid out of range")?;
    // Best effort: a full/read-only stop journal must not prevent the signal,
    // especially when --force is the operator's recovery path.
    if let Err(error) = db_lock::request_stop(&busy.paths, pid, reason, false) {
        eprintln!(
            "hya: could not record the stop request for pid {pid}; signalling anyway: {error}"
        );
    }
    // SAFETY: `kill` has no memory-safety preconditions.
    unsafe {
        libc::kill(raw, libc::SIGTERM);
    }
    if wait_released(db, timeout).await? {
        return Ok(Stopped::Stopped { pid, killed: false });
    }
    if !force {
        anyhow::bail!(
            "hya server pid {pid} did not stop within {} s; run `hya serve stop --force` to kill it",
            timeout.as_secs()
        );
    }
    // SAFETY: as above.
    unsafe {
        libc::kill(raw, libc::SIGKILL);
    }
    if wait_released(db, KILL_WAIT).await? {
        return Ok(Stopped::Stopped { pid, killed: true });
    }
    anyhow::bail!("hya server pid {pid} still holds {db} after SIGKILL")
}

/// What one `hya serve restart` handoff attempt decided.
pub(crate) enum HandoffRestart {
    /// The old generation acknowledged the handoff (`queued`) and owns the
    /// rest: it closes its turns at their durable boundaries, spawns the
    /// successor (the journal's recorded executable), releases the runtime,
    /// and waits for the successor's health — parking with the lock and the
    /// listener if the successor never proves healthy. The restart command
    /// returns here; the URL is the one the successor keeps.
    Queued(Discovery),
    /// There was nothing to hand off (no holder, or a holder that does not
    /// serve yet): the caller goes to the plain stop/start path quietly.
    NotApplicable,
}

/// Longest wait for the old generation's `queued` acknowledgement. A
/// pre-handoff daemon (another hya version) never writes it.
const HANDOFF_ACK_WAIT: Duration = Duration::from_secs(10);

/// One restart attempt through the handoff protocol: record `requested`
/// (with the successor spec, including the executable this restart command
/// was invoked with), ask the holder with a `mode: "handoff"` stop request,
/// SIGTERM, and wait for its `queued` acknowledgement — the point at which
/// this command returns. The old generation does the rest on its own: it
/// closes its turns at their handoff boundaries, spawns the successor with
/// inherited descriptors, releases the runtime, and waits for the
/// successor's health (parking as the owner if the successor fails). The
/// lock and the listener never change hands on this path — they are
/// inherited — so every failure is an explicit error; the caller never
/// falls back to a destructive stop-then-start on its own.
pub(crate) async fn restart_by_handoff(
    db: &str,
    spec: &DaemonSpec,
    relay: &RelayFlags,
) -> anyhow::Result<HandoffRestart> {
    let not_applicable = || Ok::<HandoffRestart, anyhow::Error>(HandoffRestart::NotApplicable);
    let Some(paths) = db_lock::paths(db) else {
        return not_applicable();
    };
    let Some(busy) = db_lock::holder(db).context("check the database lock")? else {
        return not_applicable();
    };
    let Some(old) = busy.discovery.clone() else {
        return not_applicable();
    };
    let old_pid = busy.holder_pid.unwrap_or(old.pid);
    let journal = paths.handoff.clone();
    // The CLI returns at `queued`, while the old generation still owns the
    // staged flock and listener. Do not overwrite its journal until it has
    // observed the successor's readiness and exited: a subsequent restart
    // otherwise erases the only durable evidence of a quick ready-then-stop.
    if let Some(state) = db_lock::read_handoff(&journal) {
        match state.stage() {
            db_lock::HandoffStage::Requested
            | db_lock::HandoffStage::Queued
            | db_lock::HandoffStage::Released => {
                let requester_alive = state
                    .stage_pid(db_lock::HandoffStage::Requested)
                    .is_some_and(|pid| pid != std::process::id() && process_alive(pid));
                if requester_alive || process_alive(old_pid) {
                    anyhow::bail!(
                        "a restart handoff is already in progress (journal {}, stage {:?}); wait for it to finish",
                        journal.display(),
                        state.stage()
                    );
                }
            }
            db_lock::HandoffStage::Ready => {
                // The successor may receive SIGTERM immediately after ready.
                // Its transfer watcher then disappears with that process, so
                // `transferred` is not required evidence. Wait for the
                // predecessor that still owns staged descriptors to exit;
                // the predecessor's ready evidence makes releasing safe.
                let predecessor = state.released_pid();
                let deadline = tokio::time::Instant::now() + HANDOFF_ACK_WAIT;
                while predecessor.is_some_and(process_alive)
                    && tokio::time::Instant::now() < deadline
                {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                if predecessor.is_some_and(process_alive) {
                    anyhow::bail!(
                        "the previous handoff predecessor is still alive (journal {}, stage Ready); wait for it to finish",
                        journal.display()
                    );
                }
            }
            db_lock::HandoffStage::Transferred | db_lock::HandoffStage::Failed => {}
        }
    }
    // A stop or another requester can change the holder while we wait for
    // transfer. Never write a request addressed to a stale generation.
    if db_lock::holder(db)
        .context("recheck the database lock before restarting")?
        .is_none_or(|holder| holder.holder_pid.unwrap_or(old.pid) != old_pid)
    {
        anyhow::bail!(
            "the server changed while waiting to restart; retry against the current generation"
        );
    }
    // The successor runs the binary this restart command was invoked with
    // (its own `current_exe`), not the old daemon's: a restart right after
    // an update must bring up the new version. Validated here and again by
    // the old generation before it spawns.
    validate_successor_exe(&spec.exe)?;
    db_lock::write_handoff_stage(
        &journal,
        db_lock::HandoffStage::Requested,
        std::process::id(),
        Some(&db_lock::HandoffSpec {
            model: spec.model.clone(),
            allow_hosts: spec.allow_hosts.clone(),
            relay: relay_spec(relay),
            exe: Some(spec.exe.clone()),
            rolled_back_from: None,
        }),
        None,
    )
    .with_context(|| format!("record the handoff journal {}", journal.display()))?;
    // The stop request must be in place before the signal: the daemon reads
    // it when SIGTERM arrives. A failed write aborts before the signal —
    // a plain-signal restart would drain the server without any handoff.
    db_lock::request_stop(&paths, old_pid, hya_server::ShutdownReason::Restart, true)
        .with_context(|| {
            format!(
                "record the restart request for pid {old_pid}; the running daemon was not signalled"
            )
        })?;
    // SAFETY: `kill` has no memory-safety preconditions.
    unsafe {
        libc::kill(
            i32::try_from(old_pid).context("pid out of range")?,
            libc::SIGTERM,
        );
    }
    let state = db_lock::wait_handoff(&journal, db_lock::HandoffStage::Queued, HANDOFF_ACK_WAIT)
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "the running daemon (pid {old_pid}) did not acknowledge the handoff within \
                 {} s; it predates the handoff protocol or was lost. Replace it explicitly: \
                 `hya serve stop` and start again",
                HANDOFF_ACK_WAIT.as_secs()
            )
        })?;
    if state.stage() == db_lock::HandoffStage::Failed {
        anyhow::bail!(
            "the running daemon rejected the handoff restart: {}",
            state.error.as_deref().unwrap_or("no reason given")
        );
    }
    Ok(HandoffRestart::Queued(old))
}

/// The JSON `hya serve restart --json` prints once the running daemon
/// acknowledged the handoff (`queued`): `url` is the one the successor
/// keeps, `startedAt` is inherited by it unchanged, and `pid` is the
/// generation that acknowledged (the successor's own pid is a matter for
/// `hya serve status`).
pub(crate) fn queued_json(found: &Discovery, db: &str) -> serde_json::Value {
    serde_json::json!({
        "url": found.url,
        "pid": found.pid,
        "version": found.version,
        "startedAt": found.started_at,
        "db": db,
        "log": log_path(db).map(|path| path.to_string_lossy().into_owned()),
        "started": false,
        "queued": true,
    })
}

/// The human line `hya serve restart` prints at the `queued` ack.
pub(crate) fn queued_line(found: &Discovery, db: &str) -> String {
    format!(
        "restart queued: hya server pid {} at {} is handing off to a new generation \
         (same URL, db {}); `hya serve status` shows the successor",
        found.pid, found.url, db
    )
}

/// Poll until nothing holds `db`'s lock; `false` on timeout.
async fn wait_released(db: &str, timeout: Duration) -> anyhow::Result<bool> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if db_lock::holder(db)
            .context("check the database lock")?
            .is_none()
        {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// `1h 2m 3s` (largest two units shown).
pub(crate) fn human_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (days, hours, minutes, seconds) = (
        total / 86_400,
        total / 3_600 % 24,
        total / 60 % 60,
        total % 60,
    );
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{seconds}s"),
        (0, 0, _) => format!("{minutes}m {seconds}s"),
        (0, _, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

/// Milliseconds since `started_at` (unix ms).
pub(crate) fn uptime(started_at: u64) -> Duration {
    Duration::from_millis(unix_ms().saturating_sub(started_at))
}

/// The JSON `hya serve start --json` / `restart --json` print.
pub(crate) fn ready_json(ready: &Ready, db: &str) -> serde_json::Value {
    serde_json::json!({
        "url": ready.discovery.url,
        "pid": ready.discovery.pid,
        "version": ready.discovery.version,
        "startedAt": ready.discovery.started_at,
        "db": db,
        "log": log_path(db).map(|path| path.to_string_lossy().into_owned()),
        "started": ready.started,
    })
}

/// The human line `hya serve start` / `restart` print.
pub(crate) fn ready_line(ready: &Ready, db: &str) -> String {
    let found = &ready.discovery;
    if ready.started {
        format!(
            "started hya server pid {} at {} (db {}, log {})",
            found.pid,
            found.url,
            db,
            log_path(db).map_or_else(String::new, |path| path.display().to_string())
        )
    } else {
        format!(
            "hya server pid {} already running at {} (hya {}, db {})",
            found.pid, found.url, found.version, db
        )
    }
}

/// A note when the running server is another hya version than this binary.
pub(crate) fn version_note(found: &Discovery) -> Option<String> {
    (found.version != env!("CARGO_PKG_VERSION")).then(|| {
        format!(
            "note: the running server is hya {}, this is hya {}; run `hya serve restart` to switch",
            found.version,
            env!("CARGO_PKG_VERSION")
        )
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_as_the_two_largest_units() {
        assert_eq!(human_duration(Duration::from_secs(5)), "5s");
        assert_eq!(human_duration(Duration::from_secs(125)), "2m 5s");
        assert_eq!(human_duration(Duration::from_secs(3_725)), "1h 2m");
        assert_eq!(human_duration(Duration::from_secs(90_000)), "1d 1h");
    }

    #[test]
    fn a_version_note_only_for_another_version() {
        let mut found = Discovery {
            url: "http://127.0.0.1:1".into(),
            pid: 1,
            version: env!("CARGO_PKG_VERSION").into(),
            started_at: 0,
            relay: None,
            allow_hosts: Vec::new(),
        };
        assert_eq!(version_note(&found), None);
        found.version = "0.0.1".into();
        assert!(version_note(&found).unwrap().contains("hya serve restart"));
    }

    #[test]
    fn the_daemon_runs_in_the_home_directory_never_the_starters() {
        let home = std::env::temp_dir();
        assert_eq!(daemon_dir(Some(home.clone().into_os_string())), home);
        assert_eq!(daemon_dir(Some("".into())), PathBuf::from("/"));
        assert_eq!(daemon_dir(None), PathBuf::from("/"));
        // A HOME that does not exist would make the spawn fail.
        let missing = home.join(format!("hya-no-such-home-{}", std::process::id()));
        assert_eq!(
            daemon_dir(Some(missing.into_os_string())),
            PathBuf::from("/")
        );

        let spec = DaemonSpec {
            db: "/state/hya/sessions.db".into(),
            model: Some("hya/echo".into()),
            yolo: true,
            pure: true,
            allow_hosts: vec!["hya.example.lan".into(), "192.168.1.20".into()],
            exe: PathBuf::from("/bin/hya"),
        };
        let command = serve_command(
            &spec,
            &RelayFlags::default(),
            std::path::Path::new("/home/me"),
        );
        assert_eq!(command.get_program(), "/bin/hya");
        assert_eq!(
            command.get_current_dir(),
            Some(std::path::Path::new("/home/me"))
        );
        let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "serve",
                "--bind",
                "127.0.0.1:0",
                "--db",
                "/state/hya/sessions.db",
                "--model",
                "hya/echo",
                "--yolo",
                "--pure",
                "--allow-host",
                "hya.example.lan",
                "--allow-host",
                "192.168.1.20"
            ]
        );
        // Never the starter's relay link or bridge token.
        let removed: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        assert!(
            removed.contains(&"HYA_RELAY_LINK".to_owned()),
            "{removed:?}"
        );
        assert!(
            removed.contains(&"HYA_SERVER_TOKEN".to_owned()),
            "{removed:?}"
        );
        let plain = DaemonSpec {
            model: None,
            yolo: false,
            allow_hosts: Vec::new(),
            pure: false,
            ..spec
        };
        let command = serve_command(&plain, &RelayFlags::default(), std::path::Path::new("/"));
        let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "serve",
                "--bind",
                "127.0.0.1:0",
                "--db",
                "/state/hya/sessions.db"
            ]
        );
    }

    #[test]
    fn a_relay_daemon_gets_the_relay_flags_and_never_prints_its_link() {
        let spec = DaemonSpec {
            db: "/state/hya/sessions.db".into(),
            model: None,
            yolo: false,
            pure: false,
            allow_hosts: Vec::new(),
            exe: PathBuf::from("/bin/hya"),
        };
        let relay = RelayFlags {
            relay: Some("https://relay.example.com/hya".into()),
            relay_transport: Some("ws".into()),
            relay_ca: Some(PathBuf::from("/etc/relay-ca.pem")),
            relay_ephemeral: true,
            relay_heartbeat: Some(20),
            relay_quiet_link: false,
        };
        let command = serve_command(&spec, &relay, std::path::Path::new("/"));
        let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
        assert_eq!(
            args,
            [
                "serve",
                "--bind",
                "127.0.0.1:0",
                "--db",
                "/state/hya/sessions.db",
                "--relay",
                "https://relay.example.com/hya",
                "--relay-transport",
                "ws",
                "--relay-ca",
                "/etc/relay-ca.pem",
                "--relay-ephemeral",
                "--relay-quiet-link",
                "--relay-heartbeat",
                "20"
            ]
        );
    }

    #[tokio::test]
    async fn an_in_memory_database_cannot_have_a_daemon() {
        let spec = DaemonSpec {
            db: String::new(),
            model: None,
            yolo: false,
            pure: false,
            allow_hosts: Vec::new(),
            exe: PathBuf::from("/nonexistent/hya"),
        };
        let error = start(&spec, Duration::from_millis(10)).await.unwrap_err();
        assert!(error.to_string().contains("--db"), "{error}");
    }

    /// A rollback successor loads the first-party sources pinned with its
    /// build; an ordinary successor never inherits a pinned root from the
    /// generation that spawns it (a rolled-back daemon's own environment).
    #[test]
    fn only_a_rollback_successor_gets_the_pinned_first_party_root() {
        let spec = DaemonSpec {
            db: "/tmp/s.db".into(),
            model: None,
            yolo: false,
            pure: false,
            allow_hosts: Vec::new(),
            exe: PathBuf::from("/pin/hya"),
        };
        let spawn = |first_party_root: Option<PathBuf>| SuccessorSpawn {
            journal: PathBuf::from("/tmp/s.db.server.handoff"),
            listener_fd: 3,
            lock_fd: 4,
            extra_fd: None,
            started_at: None,
            first_party_root,
        };
        let env_of = |command: &Command| {
            command
                .get_envs()
                .find(|(key, _)| *key == hya_bundle::FIRST_PARTY_SOURCE_ROOT_ENV)
                .map(|(_, value)| value.map(std::ffi::OsStr::to_os_string))
        };
        let rollback = successor_command(
            &spec,
            &RelayFlags::default(),
            &spawn(Some(PathBuf::from("/pin/first-party"))),
            std::path::Path::new("/"),
        );
        assert_eq!(
            env_of(&rollback),
            Some(Some(std::ffi::OsString::from("/pin/first-party")))
        );
        let ordinary = successor_command(
            &spec,
            &RelayFlags::default(),
            &spawn(None),
            std::path::Path::new("/"),
        );
        assert_eq!(env_of(&ordinary), Some(None), "the variable is removed");
    }
}
