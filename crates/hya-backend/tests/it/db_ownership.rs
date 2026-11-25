//! Ownership regressions exercising separate OS processes and the real SQLite file.
#![cfg(unix)]

use std::fs::{OpenOptions, TryLockError};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use crate::common::{Scratch, TestResult, run_bounded, wait_until};

const T10: Duration = Duration::from_secs(10);

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn hya_db(s: &Scratch, db: &Path) -> Command {
    let mut cmd = s.hya();
    cmd.arg("--db").arg(db);
    cmd
}

fn run(s: &Scratch, db: &Path, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    run_bounded(hya_db(s, db).args(args), T10)
}

fn spawn_serve(s: &Scratch, db: &Path) -> Result<Process, std::io::Error> {
    Ok(Process(
        hya_db(s, db)
            .args(["serve", "--bind", "127.0.0.1:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    ))
}

fn sidecar(s: &Scratch, suffix: &str) -> PathBuf {
    s.0.join(format!("sessions.db.{suffix}"))
}

fn owner_pid(path: &Path) -> Option<u32> {
    let file = OpenOptions::new().read(true).write(true).open(path).ok()?;
    if !matches!(file.try_lock(), Err(TryLockError::WouldBlock)) {
        return None;
    }
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn block_migration(db: &Path) -> Result<Process, Box<dyn std::error::Error>> {
    let mut child = Process(
        Command::new("python3")
            .args(["-u", "-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('BEGIN IMMEDIATE'); print('locked', flush=True); sys.stdin.readline(); c.rollback()"])
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?,
    );
    let mut line = String::new();
    BufReader::new(child.0.stdout.take().ok_or("no stdout")?).read_line(&mut line)?;
    assert_eq!(line.trim(), "locked");
    Ok(child)
}

#[test]
fn stale_live_pid_is_never_signalled_and_readonly_never_creates_missing_db() -> TestResult {
    let s = Scratch::new("hya-own-stale")?;
    let db = s.0.join("sessions.db");
    let sid = hya_proto::SessionId::new().to_string();
    for args in [&["sessions"][..], &["tail-session", &sid][..]] {
        let res = run(&s, &db, args)?;
        assert!(
            !res.status.success() && !db.exists() && !sidecar(&s, "runtime-owner.lock").exists()
        );
        assert!(String::from_utf8_lossy(&res.stderr).contains("sessions.db"));
    }
    let mut unrelated = Process(Command::new("sleep").arg("60").spawn()?);
    std::fs::write(
        sidecar(&s, "runtime-owner.lock"),
        format!("{}\n", unrelated.0.id()),
    )?;
    assert!(
        run(&s, &db, &["serve", "stop", "--timeout", "1"])?
            .status
            .success()
    );
    assert!(
        unrelated.0.try_wait()?.is_none(),
        "stop signalled an unrelated live process"
    );
    assert!(
        run(&s, &db, &["exec", "stale PID ignored"])?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn an_unreaped_killed_owner_does_not_block_stop() -> TestResult {
    let s = Scratch::new("hya-own-zombie")?;
    let db = s.0.join("sessions.db");
    let mut srv = spawn_serve(&s, &db)?;
    wait_until("server.json", T10, || sidecar(&s, "server.json").exists())?;
    srv.0.kill()?;
    wait_until("lock released", T10, || {
        owner_pid(&sidecar(&s, "runtime-owner.lock")).is_none()
    })?;
    let stopped = run(&s, &db, &["serve", "stop", "--force", "--timeout", "1"])?;
    assert!(
        stopped.status.success()
            && String::from_utf8_lossy(&stopped.stdout).contains("no hya server is running")
    );
    srv.0.wait()?;
    Ok(())
}

#[tokio::test]
async fn concurrent_direct_writers_own_the_database_before_migration() -> TestResult {
    let s = Scratch::new("hya-own-race")?;
    let db = s.0.join("sessions.db");
    drop(hya_store::SessionStore::connect(&db.to_string_lossy()).await?);
    let mut blocker = block_migration(&db)?;
    let mut writers: Vec<Process> = (0..2)
        .map(|_| {
            Ok::<_, std::io::Error>(Process(
                hya_db(&s, &db)
                    .args(["exec", "concurrent"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?,
            ))
        })
        .collect::<Result<_, _>>()?;
    wait_until("owner claimed", T10, || {
        owner_pid(&sidecar(&s, "runtime-owner.lock")).is_some()
    })?;
    wait_until("loser exited 75", T10, || {
        writers.iter_mut().any(|w| {
            w.0.try_wait()
                .is_ok_and(|st| st.is_some_and(|st| st.code() == Some(75)))
        })
    })?;
    blocker
        .0
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(b"release\n")?;
    blocker.0.wait()?;
    let mut codes = Vec::new();
    for mut w in writers {
        wait_until("writer exited", T10, || {
            w.0.try_wait().is_ok_and(|st| st.is_some())
        })?;
        codes.push(w.0.wait()?.code());
    }
    codes.sort();
    assert_eq!(codes, [Some(0), Some(75)]);
    Ok(())
}

#[tokio::test]
async fn dangling_and_existing_symlinks_have_one_discoverable_owner() -> TestResult {
    for existing in [false, true] {
        let s = Scratch::new("hya-own-symlink")?;
        let (db, alias) = (s.0.join("sessions.db"), s.0.join("alias.db"));
        std::os::unix::fs::symlink("sessions.db", &alias)?;
        if existing {
            drop(hya_store::SessionStore::connect(&db.to_string_lossy()).await?);
        }
        let mut srv = spawn_serve(&s, &alias)?;
        wait_until("discovery", T10, || {
            sidecar(&s, "server.json").exists() || s.0.join("alias.db.server.json").exists()
        })?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(sidecar(&s, "runtime-owner.lock"))?;
        assert!(
            matches!(lock.try_lock(), Err(TryLockError::WouldBlock)),
            "existing={existing}"
        );
        for path in [&alias, &db] {
            let st = run(&s, path, &["serve", "status", "--json"])?;
            assert!(
                st.status.success()
                    && serde_json::from_slice::<serde_json::Value>(&st.stdout)?["pid"]
                        == srv.0.id()
            );
            assert!(
                run(&s, path, &["exec", "through symlink"])?
                    .status
                    .success()
            );
        }
        assert_eq!(
            run(&s, &db, &["serve", "--bind", "127.0.0.1:0"])?
                .status
                .code(),
            Some(75)
        );
        assert!(run(&s, &alias, &["serve", "stop"])?.status.success());
        wait_until("server exit", T10, || {
            srv.0.try_wait().is_ok_and(|st| st.is_some())
        })?;
        assert!(srv.0.wait()?.success());
    }
    Ok(())
}

#[tokio::test]
async fn stop_notice_written_during_startup_keeps_its_reason() -> TestResult {
    let s = Scratch::new("hya-own-stop-notice")?;
    let db = s.0.join("sessions.db");
    drop(hya_store::SessionStore::connect(&db.to_string_lossy()).await?);
    let mut blocker = block_migration(&db)?;
    std::fs::write(sidecar(&s, "server.stop"), r#"{"pid":1,"reason":"stop"}"#)?;
    let mut srv = spawn_serve(&s, &db)?;
    wait_until("owner lock", T10, || {
        owner_pid(&sidecar(&s, "runtime-owner.lock")) == Some(srv.0.id())
    })?;
    let cleared = !sidecar(&s, "server.stop").exists();
    std::fs::write(
        sidecar(&s, "server.stop"),
        serde_json::to_vec(&serde_json::json!({"pid": srv.0.id(), "reason": "restart"}))?,
    )?;
    blocker
        .0
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(b"release\n")?;
    blocker.0.wait()?;
    wait_until("discovery", T10, || sidecar(&s, "server.json").exists())?;
    let disc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(sidecar(&s, "server.json"))?)?;
    let mut ev = TcpStream::connect(
        disc["url"]
            .as_str()
            .ok_or("url")?
            .strip_prefix("http://")
            .ok_or("http")?,
    )?;
    ev.set_read_timeout(Some(T10))?;
    write!(
        ev,
        "GET /v1/events/stream HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n"
    )?;
    let mut head = [0; 64];
    let n = ev.read(&mut head)?;
    assert!(String::from_utf8_lossy(&head[..n]).starts_with("HTTP/1.1 200"));
    // SAFETY: unreaped child owned by this test.
    unsafe { libc::kill(i32::try_from(srv.0.id())?, libc::SIGTERM) };
    let mut rest = String::new();
    ev.read_to_string(&mut rest)?;
    wait_until("server exit", T10, || {
        srv.0.try_wait().is_ok_and(|st| st.is_some())
    })?;
    assert!(
        srv.0.wait()?.success()
            && cleared
            && rest.contains(r#""serverStopping":{"reason":"restart"}"#),
        "{rest}"
    );
    Ok(())
}
