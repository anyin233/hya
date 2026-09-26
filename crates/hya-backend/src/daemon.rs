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
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;

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
    /// The `hya` binary to run (`std::env::current_exe()`).
    pub(crate) exe: PathBuf,
    /// Working directory of the daemon (its default request directory).
    pub(crate) cwd: PathBuf,
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

fn process_alive(pid: u32) -> bool {
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
            child = Some(spawn(spec, &paths.log)?);
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

/// `hya serve --bind 127.0.0.1:0 --db <db>` in its own session, output
/// appended to `log`.
fn spawn(spec: &DaemonSpec, log: &std::path::Path) -> anyhow::Result<Child> {
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
    let mut command = Command::new(&spec.exe);
    command
        .args(["serve", "--bind", "127.0.0.1:0", "--db", &spec.db])
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(file.try_clone().context("share the daemon log")?)
        .stderr(file);
    if let Some(model) = &spec.model {
        command.args(["--model", model]);
    }
    if spec.yolo {
        command.arg("--yolo");
    }
    if spec.pure {
        command.arg("--pure");
    }
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
    if let Err(error) = db_lock::request_stop(&busy.paths, pid, reason) {
        // Still stop it: its clients then see a plain `signal`, which they
        // treat like `stop` (they do not start the next server).
        eprintln!("hya: could not record the stop reason ({error}); stopping anyway");
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
        };
        assert_eq!(version_note(&found), None);
        found.version = "0.0.1".into();
        assert!(version_note(&found).unwrap().contains("hya serve restart"));
    }

    #[tokio::test]
    async fn an_in_memory_database_cannot_have_a_daemon() {
        let spec = DaemonSpec {
            db: String::new(),
            model: None,
            yolo: false,
            pure: false,
            exe: PathBuf::from("/nonexistent/hya"),
            cwd: PathBuf::from("/"),
        };
        let error = start(&spec, Duration::from_millis(10)).await.unwrap_err();
        assert!(error.to_string().contains("--db"), "{error}");
    }
}
