//! The persistent backend daemon (ADR-0023): detached start, status, stop,
//! restart, concurrent-start arbitration, handover, and strict open failures.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::common::{Daemons, Scratch, TestResult, assert_success, run_bounded, wait_until};

fn serve(scratch: &Scratch, db: &Path, args: &[&str]) -> Command {
    let mut cmd = scratch.hya();
    cmd.arg("serve").args(args).arg("--db").arg(db);
    cmd
}

fn run(scratch: &Scratch, db: &Path, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    Ok(serve(scratch, db, args).output()?)
}

fn json(out: &Output) -> Result<Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(&out.stdout)?)
}

fn pid_of(v: &Value) -> Result<i32, Box<dyn std::error::Error>> {
    Ok(i32::try_from(v["pid"].as_i64().ok_or("no pid")?)?)
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks process existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn authority(url: &str) -> Result<&str, Box<dyn std::error::Error>> {
    Ok(url
        .strip_prefix("http://")
        .ok_or("not http")?
        .trim_end_matches('/'))
}

fn healthy(url: &str) -> bool {
    let Ok(addr) = authority(url) else {
        return false;
    };
    let Ok(mut s) = TcpStream::connect(addr) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    if write!(
        s,
        "GET /v2/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .is_err()
    {
        return false;
    }
    let mut text = String::new();
    s.read_to_string(&mut text).is_ok() && text.contains("\"ok\":true")
}

fn open_events(url: &str) -> Result<TcpStream, Box<dyn std::error::Error>> {
    let mut s = TcpStream::connect(authority(url)?)?;
    s.set_read_timeout(Some(Duration::from_secs(15)))?;
    write!(
        s,
        "GET /v2/events/stream HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n\r\n"
    )?;
    let mut head = [0u8; 64];
    let n = s.read(&mut head)?;
    assert!(String::from_utf8_lossy(&head[..n]).starts_with("HTTP/1.1 200"));
    Ok(s)
}

fn rest_of(mut s: TcpStream) -> String {
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

#[test]
fn start_runs_a_detached_daemon_that_status_reports_and_stop_ends() -> TestResult {
    let scratch = Scratch::new("hya-daemon")?;
    let db = scratch.0.join("s.db");
    let mut daemons = Daemons(Vec::new());
    // Unlocked stale lock file naming a live process must not block start.
    std::fs::write(
        format!("{}.runtime-owner.lock", db.display()),
        format!("{}\n", std::process::id()),
    )?;

    let started = run(&scratch, &db, &["start", "--json"])?;
    assert_success("serve start", &started);
    let info = json(&started)?;
    let pid = pid_of(&info)?;
    daemons.0.push(pid);
    let url = info["url"].as_str().ok_or("no url")?.to_string();
    assert_eq!(
        (&info["started"], &info["version"]),
        (
            &Value::Bool(true),
            &Value::String(env!("CARGO_PKG_VERSION").into())
        )
    );
    assert_eq!(info["db"].as_str(), Some(db.to_string_lossy().as_ref()));
    assert!(
        url.starts_with("http://127.0.0.1:")
            && PathBuf::from(format!("{}.server.log", db.display())).is_file()
    );
    // SAFETY: getsid only reads process attributes.
    assert!(alive(pid) && unsafe { libc::getsid(0) != libc::getsid(pid) } && healthy(&url));

    let again = json(&run(&scratch, &db, &["start", "--json"])?)?;
    assert!(again["started"] == false && pid_of(&again)? == pid);
    let human = run(&scratch, &db, &["status"])?;
    assert_success("status", &human);
    let text = String::from_utf8_lossy(&human.stdout);
    for needle in [
        url.as_str(),
        &format!("pid {pid}"),
        env!("CARGO_PKG_VERSION"),
        "uptime",
    ] {
        assert!(text.contains(needle), "status lacks {needle}: {text}");
    }
    assert!(
        json(&run(&scratch, &db, &["status", "--json"])?)?["uptimeMs"]
            .as_u64()
            .is_some()
    );

    let stream = open_events(&url)?;
    let stopped = run(&scratch, &db, &["stop"])?;
    assert_success("stop", &stopped);
    let stop_out = String::from_utf8_lossy(&stopped.stdout);
    assert!(
        stop_out.contains(&format!("pid {pid}"))
            && stop_out.contains("stay disconnected until /reconnect")
    );
    assert!(rest_of(stream).contains(r#""serverStopping":{"reason":"stop"}"#));
    wait_until("daemon exited", Duration::from_secs(5), || !alive(pid))?;
    assert!(
        !PathBuf::from(format!("{}.server.json", db.display())).exists()
            && !PathBuf::from(format!("{}.server.stop", db.display())).exists()
    );
    assert!(!run(&scratch, &db, &["status"])?.status.success());
    let idle = run(&scratch, &db, &["stop"])?;
    assert!(
        idle.status.success()
            && String::from_utf8_lossy(&idle.stdout).contains("no hya server is running")
    );
    Ok(())
}

#[test]
fn restart_replaces_the_daemon() -> TestResult {
    let scratch = Scratch::new("hya-daemon-restart")?;
    let db = scratch.0.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let first = json(&run(&scratch, &db, &["start", "--json"])?)?;
    let first_pid = pid_of(&first)?;
    daemons.0.push(first_pid);
    let stream = open_events(first["url"].as_str().ok_or("no url")?)?;

    let restarted = run(&scratch, &db, &["restart", "--json"])?;
    assert_success("restart", &restarted);
    let ready = json(&restarted)?;
    let new_url = ready["url"].as_str().ok_or("no restart url")?;
    assert!(ready["restarted"] == true && pid_of(&ready)? != first_pid);
    assert!(rest_of(stream).contains(r#""serverStopping":{"reason":"restart"}"#));

    let deadline = Instant::now() + Duration::from_secs(20);
    let second_pid = loop {
        if let Ok(out) = run(&scratch, &db, &["status", "--json"])
            && out.status.success()
        {
            let st = json(&out)?;
            if pid_of(&st)? != first_pid && st["url"].as_str() == Some(new_url) && healthy(new_url)
            {
                break pid_of(&st)?;
            }
        }
        if Instant::now() > deadline {
            return Err("timed out waiting for successor".into());
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    daemons.0.push(second_pid);
    wait_until("old exited", Duration::from_secs(10), || !alive(first_pid))?;
    assert!(run(&scratch, &db, &["stop"])?.status.success());
    wait_until("new exited", Duration::from_secs(5), || !alive(second_pid))?;
    Ok(())
}

#[test]
fn concurrent_starts_share_one_daemon() -> TestResult {
    let scratch = Scratch::new("hya-daemon-race")?;
    let db = scratch.0.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let starters: Vec<_> = (0..3)
        .map(|_| {
            serve(&scratch, &db, &["start", "--json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        })
        .collect::<Result<_, _>>()?;
    let mut infos = Vec::new();
    for s in starters {
        let out = s.wait_with_output()?;
        assert_success("concurrent start", &out);
        infos.push(json(&out)?);
    }
    let pid = pid_of(&infos[0])?;
    daemons.0.push(pid);
    assert!(infos.iter().all(|i| pid_of(i).ok() == Some(pid)));
    assert_eq!(infos.iter().filter(|i| i["started"] == true).count(), 1);
    assert!(run(&scratch, &db, &["stop"])?.status.success());
    wait_until("exited", Duration::from_secs(5), || !alive(pid))?;
    Ok(())
}

#[test]
fn start_waits_out_a_server_that_is_shutting_down() -> TestResult {
    let scratch = Scratch::new("hya-daemon-handover")?;
    let db = scratch.0.join("s.db");
    let mut daemons = Daemons(Vec::new());
    let first = json(&run(&scratch, &db, &["start", "--json"])?)?;
    let first_pid = pid_of(&first)?;
    daemons.0.push(first_pid);
    let stream = open_events(first["url"].as_str().ok_or("no url")?)?;
    // SAFETY: `kill` targets our daemon process.
    unsafe { libc::kill(first_pid, libc::SIGTERM) };
    assert!(rest_of(stream).contains(r#""serverStopping":{"reason":"signal"}"#));

    let next_pid = pid_of(&json(&run(&scratch, &db, &["start", "--json"])?)?)?;
    daemons.0.push(next_pid);
    assert_ne!(next_pid, first_pid);
    assert!(run(&scratch, &db, &["stop"])?.status.success());
    wait_until("exited", Duration::from_secs(5), || !alive(next_pid))?;
    Ok(())
}

#[test]
fn strict_open_rejects_unopenable_database_across_serve_and_short_commands() -> TestResult {
    let scratch = Scratch::new("hya-strict-open")?;
    let db = scratch.0.join("db-as-directory");
    std::fs::create_dir(&db)?;

    let fg = run_bounded(
        &mut serve(&scratch, &db, &["--bind", "127.0.0.1:0"]),
        Duration::from_secs(10),
    )?;
    let fg_err = String::from_utf8_lossy(&fg.stderr);
    assert!(
        !fg.status.success()
            && !fg_err.contains("running in memory")
            && fg_err.contains("open store"),
        "{fg_err}"
    );

    let det = run_bounded(
        &mut serve(&scratch, &db, &["start", "--json"]),
        Duration::from_secs(15),
    )?;
    let det_err = String::from_utf8_lossy(&det.stderr);
    let log =
        std::fs::read_to_string(scratch.0.join("db-as-directory.server.log")).unwrap_or_default();
    assert!(
        !det.status.success()
            && !det_err.contains("did not answer within")
            && !log.contains("running in memory")
    );
    assert!(det_err.contains("open store") || log.contains("open store"));

    let sid = hya_proto::SessionId::new().to_string();
    for args in [
        vec!["sessions", "archive", &sid],
        vec!["workflow", "state", "--session", &sid],
    ] {
        let out = run_bounded(
            scratch.hya().arg("--db").arg(&db).args(&args),
            Duration::from_secs(10),
        )?;
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success()
                && !err.contains("running in memory")
                && err.contains("open store"),
            "{args:?}: {err}"
        );
    }
    Ok(())
}

#[test]
fn serve_relay_failure_cleans_discovery_and_locks() -> TestResult {
    let scratch = Scratch::new("hya-serve-relay-fail")?;
    let db = scratch.0.join("s.db");
    std::fs::write(
        format!("{}.relay-identity.json", db.display()),
        "invalid identity",
    )?;
    let out = run_bounded(
        &mut serve(
            &scratch,
            &db,
            &[
                "--bind",
                "127.0.0.1:0",
                "--relay",
                "http://127.0.0.1:1",
                "--relay-quiet-link",
            ],
        ),
        Duration::from_secs(15),
    )?;
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("identity"));
    assert!(!scratch.0.join("s.db.server.json").exists());

    let next = run_bounded(
        &mut serve(&scratch, &db, &["start", "--json"]),
        Duration::from_secs(15),
    )?;
    assert_success("restart after relay fail", &next);
    let mut daemon = Daemons(vec![pid_of(&json(&next)?)?]);
    assert!(run(&scratch, &db, &["stop"])?.status.success());
    daemon.0.clear();
    Ok(())
}
