//! Every writer respects the database lock (docs/cli.md "Database lock and
//! the backend daemon", ADR-0022/0023).
//!
//! - A file `--db` that a live server holds: `hya exec`/`run` and
//!   `hya workflow use|run|state` go through that server over `/v1` (the
//!   session shows up in the server), or exit 75 when the command cannot be
//!   expressed there.
//! - A file `--db` nobody holds: the command takes `<db>.lock` for its whole
//!   run, so a `hya serve --db` started meanwhile exits 75, and so does a
//!   second writer (the holder publishes no discovery file).
//! - In-memory stores (no `--db`) and read-only commands never lock.

use std::io::{BufRead as _, BufReader, Read as _};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const EXIT_DB_IN_USE: i32 = 75;

const ECHO_WORKFLOW: &str = r#"---
kind: Workflow
name: echo-input
description: One Stage whose directive embeds the required input.
inputs:
  v: Any value.
nodes:
  capture:
    agent: general
    directive: captured={{input.v}}
---
flowchart TD
  capture
"#;

struct Scratch(PathBuf);

impl Scratch {
    fn new(prefix: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "hya-writers-{prefix}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("state"))?;
        std::fs::create_dir_all(dir.join("work/.hya/workflows"))?;
        std::fs::write(
            dir.join("work/.hya/workflows/echo-input.hya.md"),
            ECHO_WORKFLOW,
        )?;
        Ok(Self(dir.canonicalize()?))
    }

    fn db(&self) -> PathBuf {
        self.0.join("state/sessions.db")
    }

    fn lock(&self) -> PathBuf {
        self.0.join("state/sessions.db.lock")
    }

    fn hya(&self) -> Command {
        let root = &self.0;
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
            .current_dir(root.join("work"))
            .stdin(Stdio::null());
        command
    }

    /// `hya --db <db> <args…>`, waited for.
    fn run(&self, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
        Ok(self.hya().arg("--db").arg(self.db()).args(args).output()?)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(output: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A running foreground `hya serve --db`: killed on drop.
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

/// Start `hya serve --db` and wait until it published its discovery file.
fn serve(scratch: &Scratch) -> Result<Server, Box<dyn std::error::Error>> {
    let mut child = scratch
        .hya()
        .args(["serve", "--bind", "127.0.0.1:0", "--db"])
        .arg(scratch.db())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(url) = line.strip_prefix("hya server listening on ") {
                let _ = tx.send(url.trim().to_string());
            }
        }
    });
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || for _ in BufReader::new(stderr).lines() {});
    }
    let Ok(url) = rx.recv_timeout(Duration::from_secs(90)) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("hya serve printed no readiness line".into());
    };
    let server = Server { child, url };
    let discovery = PathBuf::from(format!("{}.server.json", scratch.db().display()));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !discovery.is_file() {
        if Instant::now() > deadline {
            return Err("no discovery file".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(server)
}

/// Root session ids the server lists (archived included).
async fn server_sessions(url: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{url}/v1/sessions?includeArchived=true"))
        .send()
        .await?
        .json()
        .await?;
    Ok(body["sessions"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["parent"].as_str().is_none_or(str::is_empty))
                .filter_map(|row| row["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

#[tokio::test]
async fn exec_and_run_go_through_the_server_that_holds_the_database() -> TestResult {
    let scratch = Scratch::new("exec-routed")?;
    let server = serve(&scratch)?;

    let output = scratch.run(&["exec", "hello routed exec"])?;
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8(output.stdout.clone())?;
    // The offline provider echoes the prompt: the rendered transcript.
    assert!(stdout.contains("hello routed exec"), "{}", text(&output));
    let first = server_sessions(&server.url).await?;
    assert_eq!(
        first.len(),
        1,
        "the exec session is the server's: {first:?}"
    );

    let output = scratch.run(&["run", "--format", "json", "hello", "routed", "json"])?;
    assert!(output.status.success(), "{}", text(&output));
    let sessions = server_sessions(&server.url).await?;
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    let session = sessions
        .iter()
        .find(|id| !first.contains(id))
        .ok_or("no second session")?;
    let lines: Vec<serde_json::Value> = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert!(!lines.is_empty());
    for line in &lines {
        assert_ne!(line["seq"], serde_json::json!(0), "{line}");
    }
    assert!(
        lines.iter().any(|line| line.to_string().contains(session)),
        "the JSONL names the server's session {session}"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.to_string().contains("hello routed json")),
        "the JSONL has the echoed turn"
    );
    // The server still owns the lock: the routed commands never took it.
    let holder = std::fs::read_to_string(scratch.lock())?;
    assert_eq!(holder.trim(), server.child.id().to_string());
    drop(server);
    Ok(())
}

#[tokio::test]
async fn exec_that_cannot_be_routed_exits_75_naming_the_server() -> TestResult {
    let scratch = Scratch::new("exec-unroutable")?;
    let server = serve(&scratch)?;
    let output = scratch.run(&["--pure", "exec", "pure needs its own runtime"])?;
    assert_eq!(
        output.status.code(),
        Some(EXIT_DB_IN_USE),
        "{}",
        text(&output)
    );
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains(&server.url), "{stderr}");
    assert!(stderr.contains("hya serve stop"), "{stderr}");
    assert!(server_sessions(&server.url).await?.is_empty());
    drop(server);
    Ok(())
}

#[tokio::test]
async fn workflow_use_run_and_state_go_through_the_server() -> TestResult {
    let scratch = Scratch::new("workflow-routed")?;
    let server = serve(&scratch)?;

    let output = scratch.run(&["workflow", "run", "echo-input", "--input", "v=routed"])?;
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8(output.stdout.clone())?;
    assert!(
        stdout.contains("workflow echo-input: completed"),
        "{}",
        text(&output)
    );
    let sessions = server_sessions(&server.url).await?;
    assert_eq!(sessions.len(), 1, "the run's session is the server's");
    let session = &sessions[0];

    let output = scratch.run(&[
        "workflow",
        "use",
        "echo-input",
        "--session",
        session,
        "--json",
    ])?;
    assert!(output.status.success(), "{}", text(&output));
    let selected: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(selected["state"]["selection"]["name"], "echo-input");

    let output = scratch.run(&[
        "workflow",
        "run",
        "--session",
        session,
        "--input",
        "v=again",
        "--json",
    ])?;
    assert!(output.status.success(), "{}", text(&output));
    let run: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(run["kind"], "run", "{run}");
    assert_eq!(run["result"]["run"]["workflow"]["name"], "echo-input");
    assert_eq!(run["result"]["run"]["status"], "completed");

    let output = scratch.run(&["workflow", "state", "--session", session, "--json"])?;
    assert!(output.status.success(), "{}", text(&output));
    let state: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(state["kind"], "state");
    assert_eq!(state["state"]["run"]["status"], "completed");

    // `--revision` on a run has no /v1 field: fail fast instead.
    let output = scratch.run(&[
        "workflow",
        "run",
        "echo-input",
        "--input",
        "v=x",
        "--revision",
        &"0".repeat(64),
    ])?;
    assert_eq!(
        output.status.code(),
        Some(EXIT_DB_IN_USE),
        "{}",
        text(&output)
    );
    assert_eq!(server_sessions(&server.url).await?.len(), 1);
    drop(server);
    Ok(())
}

#[tokio::test]
async fn read_only_and_in_memory_commands_ignore_the_server() -> TestResult {
    let scratch = Scratch::new("readonly")?;
    let server = serve(&scratch)?;
    let output = scratch.run(&["exec", "seed"])?;
    assert!(output.status.success(), "{}", text(&output));
    let session = server_sessions(&server.url).await?.remove(0);

    // tail-session reads the database directly, next to the server.
    let output = scratch.run(&["tail-session", &session])?;
    assert!(output.status.success(), "{}", text(&output));
    assert!(String::from_utf8(output.stdout)?.contains("seed"));

    // Without --db, exec uses an in-memory store: nothing reaches the server.
    let output = scratch.hya().args(["exec", "in memory"]).output()?;
    assert!(output.status.success(), "{}", text(&output));
    assert!(String::from_utf8(output.stdout)?.contains("in memory"));
    // workflow list/info never open the database.
    let output = scratch.run(&["workflow", "list"])?;
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(server_sessions(&server.url).await?.len(), 1);
    drop(server);
    Ok(())
}

#[test]
fn direct_writers_record_themselves_in_the_lock_file() -> TestResult {
    let scratch = Scratch::new("direct")?;
    let child = scratch
        .hya()
        .arg("--db")
        .arg(scratch.db())
        .args(["exec", "direct"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let output = child.wait_with_output()?;
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        std::fs::read_to_string(scratch.lock())?.trim(),
        pid.to_string()
    );

    let child = scratch
        .hya()
        .arg("--db")
        .arg(scratch.db())
        .args(["workflow", "run", "echo-input", "--input", "v=direct"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let output = child.wait_with_output()?;
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        std::fs::read_to_string(scratch.lock())?.trim(),
        pid.to_string()
    );
    Ok(())
}

/// A provider endpoint that accepts requests and never answers, so a turn
/// stays in flight until the client is stopped.
fn stalled_provider() -> Result<(String, mpsc::Receiver<()>), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/v1", listener.local_addr()?);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().map_while(Result::ok) {
            let mut stream = stream;
            let mut buffer = [0u8; 4096];
            let _ = stream.read(&mut buffer);
            let _ = tx.send(());
            held.push(stream);
        }
    });
    Ok((url, rx))
}

#[test]
fn a_direct_writer_holds_the_lock_for_its_whole_run() -> TestResult {
    let scratch = Scratch::new("held")?;
    let (provider, requested) = stalled_provider()?;
    std::fs::create_dir_all(scratch.0.join("config/hya"))?;
    std::fs::write(
        scratch.0.join("config/hya/config.yaml"),
        format!(
            "default_model: stall/m\nproviders:\n  stall:\n    kind: openai-completion\n    base_url: {provider}\n    api_key: test\n    models: [m]\n"
        ),
    )?;
    let mut exec = scratch
        .hya()
        .arg("--db")
        .arg(scratch.db())
        .args(["exec", "never answered"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| -> TestResult {
        requested
            .recv_timeout(Duration::from_secs(90))
            .map_err(|_| "exec never reached the provider")?;
        let pid = exec.id().to_string();

        let output = scratch
            .hya()
            .args(["serve", "--bind", "127.0.0.1:0", "--db"])
            .arg(scratch.db())
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(EXIT_DB_IN_USE),
            "{}",
            text(&output)
        );
        assert!(String::from_utf8(output.stderr)?.contains(&pid));

        for args in [
            &["exec", "second writer"][..],
            &["workflow", "run", "echo-input", "--input", "v=x"][..],
        ] {
            let output = scratch.run(args)?;
            assert_eq!(
                output.status.code(),
                Some(EXIT_DB_IN_USE),
                "{}",
                text(&output)
            );
            let stderr = String::from_utf8(output.stderr)?;
            assert!(stderr.contains("does not serve HTTP yet"), "{stderr}");
            assert!(stderr.contains(&pid), "{stderr}");
        }
        Ok(())
    })();
    let _ = Command::new("kill")
        .args(["-INT", &exec.id().to_string()])
        .status();
    let deadline = Instant::now() + Duration::from_secs(20);
    while exec.try_wait()?.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = exec.kill();
    let _ = exec.wait();
    result
}
