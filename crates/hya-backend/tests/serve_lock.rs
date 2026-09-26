//! One writer per database (docs/cli.md "`hya serve`", ADR-0022): `hya serve
//! --db <db>` holds an exclusive lock on `<db>.lock` for its lifetime and
//! publishes `<db>.server.json` once listening. A second `hya serve` on the
//! same database fails fast with exit status 75 and names the running
//! server; a stale discovery file (its process gone, the lock free) is
//! ignored and overwritten; a clean shutdown removes the discovery file.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scratch(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn serve(root: &Path, db: &Path) -> Command {
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
        .args(["serve", "--bind", "127.0.0.1:0", "--db"])
        .arg(db)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// A running `hya serve`: killed on drop.
struct Server {
    child: Child,
    url: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start `hya serve` and wait for its readiness line.
fn start(root: &Path, db: &Path) -> Result<Server, Box<dyn std::error::Error>> {
    let mut child = serve(root, db).spawn()?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(url) = line.strip_prefix("hya server listening on ") {
                let _ = tx.send(url.trim().to_string());
            }
        }
    });
    // Drain stderr so the child never blocks on a full pipe.
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || for _ in BufReader::new(stderr).lines() {});
    }
    match rx.recv_timeout(Duration::from_secs(90)) {
        Ok(url) => Ok(Server { child, url }),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err("hya serve printed no readiness line".into())
        }
    }
}

fn discovery_path(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}.server.json", db.display()))
}

fn read_discovery(db: &Path) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_str(&std::fs::read_to_string(
        discovery_path(db),
    )?)?)
}

fn terminate(server: &mut Server) -> Result<std::process::ExitStatus, Box<dyn std::error::Error>> {
    let pid = i32::try_from(server.child.id())?;
    // SAFETY: `kill` has no memory-safety preconditions; `pid` is our own
    // child and has not been reaped yet.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = server.child.try_wait()? {
            return Ok(status);
        }
        if std::time::Instant::now() > deadline {
            return Err("hya serve did not exit after SIGTERM".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn serve_locks_the_database_publishes_discovery_and_a_second_serve_fails_fast() -> TestResult {
    let root = scratch("hya-serve-lock")?;
    let db = root.join("state/sessions.db");
    std::fs::create_dir_all(db.parent().ok_or("no parent")?)?;
    let mut first = start(&root, &db)?;

    // The lock file exists and the discovery file names this server.
    assert!(
        PathBuf::from(format!("{}.lock", db.display())).is_file(),
        "no lock file next to the database"
    );
    let discovery = read_discovery(&db)?;
    assert_eq!(discovery["url"], serde_json::json!(first.url));
    assert_eq!(discovery["pid"], serde_json::json!(first.child.id()));
    assert_eq!(
        discovery["version"],
        serde_json::json!(env!("CARGO_PKG_VERSION"))
    );
    assert!(
        discovery["startedAt"].as_u64().is_some_and(|ms| ms > 0),
        "{discovery}"
    );

    // A second serve on the same database fails fast and names the first.
    let started = std::time::Instant::now();
    let second = serve(&root, &db).output()?;
    let stderr = String::from_utf8(second.stderr)?;
    assert_eq!(second.status.code(), Some(75), "stderr: {stderr}");
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(stderr.contains("already in use"), "{stderr}");
    assert!(stderr.contains(&first.url), "{stderr}");
    assert!(
        stderr.contains(&format!("pid {}", first.child.id())),
        "{stderr}"
    );
    assert!(
        !String::from_utf8(second.stdout)?.contains("hya server listening on"),
        "the second serve must not start"
    );
    // The first server's discovery file is untouched.
    assert_eq!(read_discovery(&db)?["pid"], discovery["pid"]);

    // A clean shutdown removes the discovery file and releases the lock.
    let status = terminate(&mut first)?;
    assert!(status.success(), "{status}");
    assert!(
        !discovery_path(&db).exists(),
        "discovery file left after a clean shutdown"
    );
    let third = start(&root, &db)?;
    assert_eq!(
        read_discovery(&db)?["pid"],
        serde_json::json!(third.child.id())
    );
    drop(third);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn a_stale_discovery_file_is_ignored_and_overwritten() -> TestResult {
    let root = scratch("hya-serve-stale")?;
    let db = root.join("sessions.db");
    // A crashed server left its discovery file; nothing holds the lock.
    std::fs::write(
        discovery_path(&db),
        r#"{"url":"http://127.0.0.1:1","pid":999999,"version":"0.0.0","startedAt":1}"#,
    )?;
    std::fs::write(format!("{}.lock", db.display()), "999999\n")?;
    let server = start(&root, &db)?;
    let discovery = read_discovery(&db)?;
    assert_eq!(discovery["url"], serde_json::json!(server.url));
    assert_eq!(discovery["pid"], serde_json::json!(server.child.id()));
    drop(server);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn an_in_memory_serve_takes_no_lock() -> TestResult {
    let root = scratch("hya-serve-memory")?;
    let mut command = serve(&root, Path::new(""));
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line)?;
    let _ = child.kill();
    let _ = child.wait();
    assert!(line.starts_with("hya server listening on "), "{line}");
    assert!(!root.join(".lock").exists());
    assert!(!root.join(".server.json").exists());
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
