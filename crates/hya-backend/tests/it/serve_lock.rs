//! One writer per database (ADR-0022): exclusive lock, discovery file, exit 75
//! on conflict, stale discovery overwrite, and unlocked in-memory serve.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::common::{ForegroundServer, Scratch, TestResult};

fn discovery_path(db: &Path) -> PathBuf {
    PathBuf::from(format!("{}.server.json", db.display()))
}

fn read_discovery(db: &Path) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_str(&std::fs::read_to_string(
        discovery_path(db),
    )?)?)
}

fn start_serve(
    scratch: &Scratch,
    db: &Path,
) -> Result<ForegroundServer, Box<dyn std::error::Error>> {
    let mut cmd = scratch.hya();
    cmd.args(["serve", "--bind", "127.0.0.1:0", "--db"]).arg(db);
    ForegroundServer::start(cmd, "hya server listening on ", Some(&discovery_path(db)))
}

#[test]
fn serve_locks_the_database_publishes_discovery_and_a_second_serve_fails_fast() -> TestResult {
    let scratch = Scratch::new("hya-serve-lock")?;
    let db = scratch.db();
    std::fs::create_dir_all(db.parent().ok_or("no parent")?)?;
    let mut first = start_serve(&scratch, &db)?;

    assert!(PathBuf::from(format!("{}.runtime-owner.lock", db.display())).is_file());
    let discovery = read_discovery(&db)?;
    assert_eq!(discovery["url"], serde_json::json!(first.url));
    assert_eq!(discovery["pid"], serde_json::json!(first.child.id()));
    assert_eq!(
        discovery["version"],
        serde_json::json!(env!("CARGO_PKG_VERSION"))
    );
    assert!(discovery["startedAt"].as_u64().is_some_and(|ms| ms > 0));

    let started = Instant::now();
    let second = scratch
        .hya()
        .args(["serve", "--bind", "127.0.0.1:0", "--db"])
        .arg(&db)
        .output()?;
    let stderr = String::from_utf8(second.stderr)?;
    assert_eq!(second.status.code(), Some(75), "stderr: {stderr}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(stderr.contains("already in use") && stderr.contains(&first.url));
    assert!(stderr.contains(&format!("pid {}", first.child.id())));
    assert!(!String::from_utf8(second.stdout)?.contains("hya server listening on"));
    assert_eq!(read_discovery(&db)?["pid"], discovery["pid"]);

    let status = first.terminate()?;
    assert!(status.success(), "{status}");
    assert!(!discovery_path(&db).exists());
    let third = start_serve(&scratch, &db)?;
    assert_eq!(
        read_discovery(&db)?["pid"],
        serde_json::json!(third.child.id())
    );
    Ok(())
}

#[test]
fn a_stale_discovery_file_is_ignored_and_overwritten() -> TestResult {
    let scratch = Scratch::new("hya-serve-stale")?;
    let db = scratch.0.join("sessions.db");
    std::fs::write(
        discovery_path(&db),
        r#"{"url":"http://127.0.0.1:1","pid":999999,"version":"0.0.0","startedAt":1}"#,
    )?;
    std::fs::write(format!("{}.runtime-owner.lock", db.display()), "999999\n")?;
    let server = start_serve(&scratch, &db)?;
    let discovery = read_discovery(&db)?;
    assert_eq!(discovery["url"], serde_json::json!(server.url));
    assert_eq!(discovery["pid"], serde_json::json!(server.child.id()));
    Ok(())
}

#[test]
fn an_in_memory_serve_takes_no_lock() -> TestResult {
    let scratch = Scratch::new("hya-serve-memory")?;
    let mut child = scratch
        .hya()
        .args(["serve", "--bind", "127.0.0.1:0", "--db", ""])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take().ok_or("no stdout")?).read_line(&mut line)?;
    let _ = child.kill();
    let _ = child.wait();
    assert!(line.starts_with("hya server listening on "), "{line}");
    assert!(!scratch.0.join(".lock").exists() && !scratch.0.join(".server.json").exists());
    Ok(())
}
