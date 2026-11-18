//! Ownership regressions exercise separate processes and the real SQLite file.
#![cfg(unix)]

use std::fs::{OpenOptions, TryLockError};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("hya-ownership-{}", hya_proto::SessionId::new()));
        std::fs::create_dir_all(&root)?;
        Ok(Self(root.canonicalize()?))
    }

    fn db(&self) -> PathBuf {
        self.0.join("sessions.db")
    }

    fn sidecar(&self, suffix: &str) -> PathBuf {
        self.0.join(format!("sessions.db.{suffix}"))
    }

    fn hya(&self, db: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.0.join("home"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .current_dir(&self.0)
            .arg("--db")
            .arg(db)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_until(mut ready: impl FnMut() -> bool) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        if Instant::now() >= deadline {
            return Err("timed out waiting for ownership test condition".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn output(command: &mut Command) -> Result<Output, Box<dyn std::error::Error>> {
    let mut child = Process(command.spawn()?);
    wait_until(|| child.0.try_wait().is_ok_and(|status| status.is_some()))?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    child
        .0
        .stdout
        .take()
        .ok_or("no stdout")?
        .read_to_end(&mut stdout)?;
    child
        .0
        .stderr
        .take()
        .ok_or("no stderr")?
        .read_to_end(&mut stderr)?;
    Ok(Output {
        status: child.0.wait()?,
        stdout,
        stderr,
    })
}

fn owner_pid(path: &Path) -> Option<u32> {
    let file = OpenOptions::new().read(true).write(true).open(path).ok()?;
    if !matches!(file.try_lock(), Err(TryLockError::WouldBlock)) {
        return None;
    }
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[test]
fn stale_live_pid_is_not_an_owner_and_is_never_signalled() -> TestResult {
    let scratch = Scratch::new()?;
    let mut unrelated = Process(Command::new("sleep").arg("60").spawn()?);
    std::fs::write(
        scratch.sidecar("runtime-owner.lock"),
        format!("{}\n", unrelated.0.id()),
    )?;
    let stopped = output(
        scratch
            .hya(&scratch.db())
            .args(["serve", "stop", "--timeout", "1"]),
    )?;
    assert!(
        unrelated.0.try_wait()?.is_none(),
        "stop signalled an unrelated live process"
    );
    assert!(stopped.status.success(), "{stopped:?}");
    let direct = output(
        scratch
            .hya(&scratch.db())
            .args(["exec", "stale PID ignored"]),
    )?;
    assert!(direct.status.success(), "{direct:?}");
    Ok(())
}

#[test]
fn an_unreaped_killed_owner_does_not_block_stop() -> TestResult {
    let scratch = Scratch::new()?;
    let mut server = Process(
        scratch
            .hya(&scratch.db())
            .args(["serve", "--bind", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .spawn()?,
    );
    wait_until(|| scratch.sidecar("server.json").exists())?;
    server.0.kill()?;
    // Deliberately do not reap: kill(pid, 0) still succeeds for this zombie.
    wait_until(|| owner_pid(&scratch.sidecar("runtime-owner.lock")).is_none())?;
    let stopped =
        output(
            scratch
                .hya(&scratch.db())
                .args(["serve", "stop", "--force", "--timeout", "1"]),
        )?;
    assert!(stopped.status.success(), "{stopped:?}");
    assert!(
        String::from_utf8_lossy(&stopped.stdout).contains("no hya server is running"),
        "{stopped:?}"
    );
    server.0.wait()?;
    Ok(())
}

#[test]
fn listing_and_tail_never_create_a_missing_database() -> TestResult {
    let scratch = Scratch::new()?;
    let session = hya_proto::SessionId::new().to_string();
    for args in [&["sessions"][..], &["tail-session", &session][..]] {
        let result = output(scratch.hya(&scratch.db()).args(args))?;
        assert!(
            !result.status.success(),
            "missing database was accepted: {result:?}"
        );
        assert!(
            !scratch.db().exists(),
            "read-only command created the database"
        );
        assert!(!scratch.sidecar("runtime-owner.lock").exists());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("sessions.db"),
            "{result:?}"
        );
    }
    Ok(())
}

/// Hold BEGIN IMMEDIATE in another process so startup cannot finish migrating.
fn block_migration(db: &Path) -> Result<Process, Box<dyn std::error::Error>> {
    let mut child = Process(Command::new("python3")
        .args(["-u", "-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('BEGIN IMMEDIATE'); print('locked', flush=True); sys.stdin.readline(); c.rollback()"])
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?);
    let mut line = String::new();
    BufReader::new(child.0.stdout.take().ok_or("no blocker stdout")?).read_line(&mut line)?;
    assert_eq!(line.trim(), "locked");
    Ok(child)
}

#[tokio::test]
async fn concurrent_direct_writers_own_the_database_before_migration() -> TestResult {
    let scratch = Scratch::new()?;
    drop(hya_store::SessionStore::connect(&scratch.db().to_string_lossy()).await?);
    let mut blocker = block_migration(&scratch.db())?;
    let mut writers = Vec::new();
    for _ in 0..2 {
        writers.push(Process(
            scratch
                .hya(&scratch.db())
                .args(["exec", "concurrent writer"])
                .spawn()?,
        ));
    }
    // Both writers are now competing while SQLite's migration transaction is blocked.
    wait_until(|| owner_pid(&scratch.sidecar("runtime-owner.lock")).is_some())?;
    // Keep the winner blocked until the loser has observed its ownership;
    // otherwise both short runs can legitimately succeed in sequence.
    wait_until(|| {
        writers.iter_mut().any(|writer| {
            writer
                .0
                .try_wait()
                .is_ok_and(|status| status.is_some_and(|status| status.code() == Some(75)))
        })
    })?;
    blocker
        .0
        .stdin
        .take()
        .ok_or("no blocker stdin")?
        .write_all(b"release\n")?;
    blocker.0.wait()?;
    let mut statuses = Vec::new();
    for mut writer in writers {
        wait_until(|| writer.0.try_wait().is_ok_and(|status| status.is_some()))?;
        statuses.push(writer.0.wait()?.code());
    }
    statuses.sort();
    assert_eq!(
        statuses,
        [Some(0), Some(75)],
        "the loser must not open SQLite or exit 1"
    );
    Ok(())
}

async fn symlinked_database_owner(existing: bool) -> TestResult {
    let scratch = Scratch::new()?;
    let alias = scratch.0.join("alias.db");
    std::os::unix::fs::symlink("sessions.db", &alias)?;
    if existing {
        drop(hya_store::SessionStore::connect(&scratch.db().to_string_lossy()).await?);
    }
    let mut server = Process(
        scratch
            .hya(&alias)
            .args(["serve", "--bind", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .spawn()?,
    );
    let ready = wait_until(|| {
        scratch.sidecar("server.json").exists() || scratch.0.join("alias.db.server.json").exists()
    });
    ready?;
    let canonical_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(scratch.sidecar("runtime-owner.lock"))?;
    assert!(
        matches!(canonical_lock.try_lock(), Err(TryLockError::WouldBlock)),
        "alias server did not own the target database lock (existing={existing})"
    );
    for path in [&alias, &scratch.db()] {
        let status = output(scratch.hya(path).args(["serve", "status", "--json"]))?;
        assert!(status.status.success(), "{status:?}");
        let status: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        assert_eq!(status["pid"], server.0.id());
        let routed = output(scratch.hya(path).args(["exec", "through symlink"]))?;
        assert!(routed.status.success(), "{routed:?}");
    }
    let second = output(
        scratch
            .hya(&scratch.db())
            .args(["serve", "--bind", "127.0.0.1:0"]),
    )?;
    assert_eq!(second.status.code(), Some(75), "{second:?}");
    let stopped = output(scratch.hya(&alias).args(["serve", "stop"]))?;
    assert!(stopped.status.success(), "{stopped:?}");
    wait_until(|| server.0.try_wait().is_ok_and(|status| status.is_some()))?;
    assert!(server.0.wait()?.success());
    Ok(())
}

#[tokio::test]
async fn dangling_symlink_has_one_discoverable_database_owner() -> TestResult {
    symlinked_database_owner(false).await
}

#[tokio::test]
async fn existing_symlink_has_one_discoverable_database_owner() -> TestResult {
    symlinked_database_owner(true).await
}

#[tokio::test]
async fn stop_notice_written_during_startup_keeps_its_reason() -> TestResult {
    let scratch = Scratch::new()?;
    drop(hya_store::SessionStore::connect(&scratch.db().to_string_lossy()).await?);
    let mut blocker = block_migration(&scratch.db())?;
    std::fs::write(
        scratch.sidecar("server.stop"),
        r#"{"pid":1,"reason":"stop"}"#,
    )?;
    let mut server = Process(
        scratch
            .hya(&scratch.db())
            .args(["serve", "--bind", "127.0.0.1:0"])
            .spawn()?,
    );
    wait_until(|| owner_pid(&scratch.sidecar("runtime-owner.lock")) == Some(server.0.id()))?;
    let cleared_at_acquisition = !scratch.sidecar("server.stop").exists();
    // The CLI writes this request before signalling. Delay the signal so we can
    // subscribe and inspect the shutdown reason after the blocked startup.
    std::fs::write(
        scratch.sidecar("server.stop"),
        serde_json::to_vec(&serde_json::json!({
            "pid": server.0.id(), "reason": "restart"
        }))?,
    )?;
    blocker
        .0
        .stdin
        .take()
        .ok_or("no blocker stdin")?
        .write_all(b"release\n")?;
    blocker.0.wait()?;
    wait_until(|| scratch.sidecar("server.json").exists())?;
    let discovery: serde_json::Value =
        serde_json::from_slice(&std::fs::read(scratch.sidecar("server.json"))?)?;
    let authority = discovery["url"]
        .as_str()
        .ok_or("no URL")?
        .strip_prefix("http://")
        .ok_or("not HTTP")?;
    let mut events = TcpStream::connect(authority)?;
    events.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        events,
        "GET /v1/events/stream HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n"
    )?;
    let mut head = [0; 64];
    let count = events.read(&mut head)?;
    assert!(String::from_utf8_lossy(&head[..count]).starts_with("HTTP/1.1 200"));
    // SAFETY: the unreaped child is this test's server.
    unsafe {
        libc::kill(i32::try_from(server.0.id())?, libc::SIGTERM);
    }
    let mut rest = String::new();
    events.read_to_string(&mut rest)?;
    wait_until(|| server.0.try_wait().is_ok_and(|status| status.is_some()))?;
    assert!(server.0.wait()?.success());
    assert!(
        rest.contains(r#""serverStopping":{"reason":"restart"}"#),
        "{rest}"
    );
    assert!(
        cleared_at_acquisition,
        "stale notice was not cleared when the owner lock was acquired"
    );
    Ok(())
}
