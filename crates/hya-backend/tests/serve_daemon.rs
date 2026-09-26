//! The persistent backend daemon (ADR-0023; docs/cli.md "`hya serve`
//! daemon control"): `hya serve start` runs `hya serve` detached (its own
//! session, output to `<db>.server.log`) and returns once it answers; the
//! daemon outlives the starter and every client. `status` reports it from the
//! discovery file plus a health probe, `stop` ends it gracefully (even with
//! clients still streaming), and `restart` replaces it. Concurrent starters
//! end up on one daemon (the database lock arbitrates).

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scratch(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir.canonicalize()?)
}

/// `hya serve <args> --db <db>` with an isolated HOME/XDG under `root`.
fn serve(root: &Path, db: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("NO_COLOR", "1")
        .current_dir(root)
        .arg("serve")
        .args(args)
        .arg("--db")
        .arg(db)
        .stdin(Stdio::null());
    command
}

fn run(root: &Path, db: &Path, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    Ok(serve(root, db, args).output()?)
}

fn json(output: &Output) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.trim()).map_err(|error| {
        format!(
            "not JSON ({error}): {text}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into()
    })
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn pid_of(value: &serde_json::Value) -> Result<i32, Box<dyn std::error::Error>> {
    Ok(i32::try_from(value["pid"].as_i64().ok_or("no pid")?)?)
}

/// Kills every daemon a test started, even when it fails half-way.
struct Daemons(Vec<i32>);

impl Drop for Daemons {
    fn drop(&mut self) {
        for pid in &self.0 {
            // SAFETY: `kill` has no memory-safety preconditions.
            unsafe {
                libc::kill(*pid, libc::SIGKILL);
            }
        }
    }
}

/// `host:port` of `http://host:port`.
fn authority(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    Ok(url
        .strip_prefix("http://")
        .ok_or("not http")?
        .trim_end_matches('/')
        .to_string())
}

/// `GET <url><path>` over a raw connection; the status line and body.
fn http_get(url: &str, path: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut socket = TcpStream::connect(authority(url)?)?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        socket,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )?;
    let mut text = String::new();
    socket.read_to_string(&mut text)?;
    Ok(text)
}

/// Open `GET /v1/events/stream` (SSE) and wait for its response head.
fn open_events(url: &str) -> Result<TcpStream, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(authority(url)?)?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    write!(
        stream,
        "GET /v1/events/stream HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n"
    )?;
    let mut head = [0u8; 64];
    let read = stream.read(&mut head)?;
    assert!(String::from_utf8_lossy(&head[..read]).starts_with("HTTP/1.1 200"));
    Ok(stream)
}

/// Everything the server still sends on `stream` until it closes it.
fn rest_of(mut stream: TcpStream) -> String {
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn wait_until(what: &str, timeout: Duration, mut check: impl FnMut() -> bool) -> TestResult {
    let deadline = Instant::now() + timeout;
    while !check() {
        if Instant::now() > deadline {
            return Err(format!("timed out waiting until {what}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

#[test]
fn start_runs_a_detached_daemon_that_status_reports_and_stop_ends() -> TestResult {
    let root = scratch("hya-daemon")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());

    let started = run(&root, &db, &["start", "--json"])?;
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let info = json(&started)?;
    let pid = pid_of(&info)?;
    daemons.0.push(pid);
    assert_eq!(info["started"], serde_json::json!(true));
    assert_eq!(
        info["version"],
        serde_json::json!(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(info["db"].as_str(), Some(db.to_string_lossy().as_ref()));
    let url = info["url"].as_str().ok_or("no url")?.to_string();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    let log = PathBuf::from(format!("{}.server.log", db.display()));
    assert_eq!(info["log"].as_str(), Some(log.to_string_lossy().as_ref()));

    // The starter has exited; the daemon runs on, in its own session.
    assert!(alive(pid));
    // SAFETY: getsid only reads process attributes.
    let (ours, theirs) = unsafe { (libc::getsid(0), libc::getsid(pid)) };
    assert_ne!(
        ours, theirs,
        "the daemon must not share the starter's session"
    );
    assert!(http_get(&url, "/v1/health")?.contains("\"ok\":true"));
    assert!(log.is_file(), "the daemon logs to {}", log.display());

    // A second start finds it.
    let again = json(&run(&root, &db, &["start", "--json"])?)?;
    assert_eq!(again["started"], serde_json::json!(false));
    assert_eq!(pid_of(&again)?, pid);
    assert_eq!(again["url"].as_str(), Some(url.as_str()));

    // Status: human and JSON.
    let status = run(&root, &db, &["status"])?;
    assert!(status.status.success());
    let text = String::from_utf8_lossy(&status.stdout);
    for needle in [
        url.as_str(),
        &format!("pid {pid}"),
        env!("CARGO_PKG_VERSION"),
        "uptime",
        &db.to_string_lossy(),
    ] {
        assert!(text.contains(needle), "status lacks {needle}: {text}");
    }
    let status = json(&run(&root, &db, &["status", "--json"])?)?;
    assert_eq!(pid_of(&status)?, pid);
    assert!(status["uptimeMs"].as_u64().is_some(), "{status}");

    // Stop: graceful, even while a client holds an event stream open.
    let stream = open_events(&url)?;
    let begun = Instant::now();
    let stopped = run(&root, &db, &["stop"])?;
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(
        begun.elapsed() < Duration::from_secs(15),
        "an open client stream must not hold the shutdown open ({:?})",
        begun.elapsed()
    );
    assert!(String::from_utf8_lossy(&stopped.stdout).contains(&format!("pid {pid}")));
    // The client was told it was a manual stop (so it does not start the
    // next daemon), and the output says so.
    let told = rest_of(stream);
    assert!(
        told.contains(r#""serverStopping":{"reason":"stop"}"#),
        "the stream's last frame names the stop: {told}"
    );
    assert!(
        String::from_utf8_lossy(&stopped.stdout).contains("stay disconnected until /reconnect"),
        "{}",
        String::from_utf8_lossy(&stopped.stdout)
    );
    assert!(
        !PathBuf::from(format!("{}.server.stop", db.display())).exists(),
        "the daemon consumed the stop request"
    );
    wait_until("the daemon exited", Duration::from_secs(5), || !alive(pid))?;
    assert!(!PathBuf::from(format!("{}.server.json", db.display())).exists());

    let status = run(&root, &db, &["status"])?;
    assert!(!status.status.success(), "status of a stopped server fails");
    assert!(String::from_utf8_lossy(&status.stderr).contains("no hya server is running"));
    // Stopping nothing is not an error.
    let idle = run(&root, &db, &["stop"])?;
    assert!(idle.status.success());
    assert!(String::from_utf8_lossy(&idle.stdout).contains("no hya server is running"));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn restart_replaces_the_daemon() -> TestResult {
    let root = scratch("hya-daemon-restart")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let first = json(&run(&root, &db, &["start", "--json"])?)?;
    let first_pid = pid_of(&first)?;
    daemons.0.push(first_pid);
    let stream = open_events(first["url"].as_str().ok_or("no url")?)?;
    let restarted = run(&root, &db, &["restart", "--json"])?;
    assert!(
        restarted.status.success(),
        "{}",
        String::from_utf8_lossy(&restarted.stderr)
    );
    let second = json(&restarted)?;
    let second_pid = pid_of(&second)?;
    daemons.0.push(second_pid);
    assert_ne!(first_pid, second_pid);
    assert_eq!(second["started"], serde_json::json!(true));
    assert!(!alive(first_pid));
    let told = rest_of(stream);
    assert!(
        told.contains(r#""serverStopping":{"reason":"restart"}"#),
        "clients of a restarted daemon wait for the next one: {told}"
    );
    assert!(
        http_get(second["url"].as_str().ok_or("no url")?, "/v1/health")?.contains("\"ok\":true")
    );
    assert!(run(&root, &db, &["stop"])?.status.success());
    wait_until("the daemon exited", Duration::from_secs(5), || {
        !alive(second_pid)
    })?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn concurrent_starts_share_one_daemon() -> TestResult {
    let root = scratch("hya-daemon-race")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let starters: Vec<_> = (0..3)
        .map(|_| {
            serve(&root, &db, &["start", "--json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        })
        .collect::<Result<_, _>>()?;
    let mut infos = Vec::new();
    for starter in starters {
        let output = starter.wait_with_output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        infos.push(json(&output)?);
    }
    let pid = pid_of(&infos[0])?;
    daemons.0.push(pid);
    for info in &infos {
        assert_eq!(
            pid_of(info)?,
            pid,
            "every starter reports the one daemon: {infos:?}"
        );
    }
    let started = infos
        .iter()
        .filter(|info| info["started"] == serde_json::json!(true))
        .count();
    assert_eq!(started, 1, "exactly one starter started it: {infos:?}");
    assert!(run(&root, &db, &["stop"])?.status.success());
    wait_until("the daemon exited", Duration::from_secs(5), || !alive(pid))?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn start_waits_out_a_server_that_is_shutting_down() -> TestResult {
    // `restart`-like sequence from two sides: a stop in flight, then a start
    // that finds the lock still held by the stopping server. The start must
    // not fail; it starts the next daemon once the lock is free.
    let root = scratch("hya-daemon-handover")?;
    let db = root.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let first = json(&run(&root, &db, &["start", "--json"])?)?;
    let first_pid = pid_of(&first)?;
    daemons.0.push(first_pid);
    let stream = open_events(first["url"].as_str().ok_or("no url")?)?;
    // SAFETY: `kill` has no memory-safety preconditions.
    unsafe {
        libc::kill(first_pid, libc::SIGTERM);
    }
    // A plain signal (no `hya serve stop` request) is reported as such.
    let told = rest_of(stream);
    assert!(
        told.contains(r#""serverStopping":{"reason":"signal"}"#),
        "{told}"
    );
    let next = run(&root, &db, &["start", "--json"])?;
    assert!(
        next.status.success(),
        "{}",
        String::from_utf8_lossy(&next.stderr)
    );
    let next = json(&next)?;
    let next_pid = pid_of(&next)?;
    daemons.0.push(next_pid);
    assert_ne!(next_pid, first_pid);
    assert!(run(&root, &db, &["stop"])?.status.success());
    wait_until("the daemon exited", Duration::from_secs(5), || {
        !alive(next_pid)
    })?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
