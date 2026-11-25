//! Every writer respects the database lock (ADR-0022/0023): routed through a
//! live server, or direct under `<db>.runtime-owner.lock`, with exit 75 when
//! unroutable or busy.

use std::io::Read as _;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::common::{ForegroundServer, Scratch, TestResult, assert_success, combined_text};

const EXIT_DB_IN_USE: i32 = 75;
const ECHO_WF: &str = "---\nkind: Workflow\nname: echo-input\ndescription: Echo.\ninputs:\n  v: Any value.\nnodes:\n  capture:\n    agent: hya-task\n    directive: captured={{input.v}}\n---\nflowchart TD\n  capture\n";

struct WriterScratch(Scratch);

impl WriterScratch {
    fn new(prefix: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let s = Scratch::new(prefix)?;
        std::fs::create_dir_all(s.0.join("state"))?;
        std::fs::create_dir_all(s.0.join("work/.hya/workflows"))?;
        std::fs::write(s.0.join("work/.hya/workflows/echo-input.hya.md"), ECHO_WF)?;
        Ok(Self(s))
    }
    fn db(&self) -> PathBuf {
        self.0.db()
    }
    fn lock(&self) -> PathBuf {
        self.0.0.join("state/sessions.db.runtime-owner.lock")
    }
    fn hya(&self) -> Command {
        let mut cmd = self.0.hya();
        cmd.current_dir(self.0.0.join("work"));
        cmd
    }
    fn run(&self, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
        Ok(self.hya().arg("--db").arg(self.db()).args(args).output()?)
    }
    fn run_json(&self, label: &str, args: &[&str]) -> Result<Value, Box<dyn std::error::Error>> {
        let out = self.run(args)?;
        assert_success(label, &out);
        Ok(serde_json::from_slice(&out.stdout)?)
    }
    fn start_server(&self) -> Result<ForegroundServer, Box<dyn std::error::Error>> {
        let mut cmd = self.hya();
        cmd.args(["serve", "--bind", "127.0.0.1:0", "--db"])
            .arg(self.db());
        let disc = PathBuf::from(format!("{}.server.json", self.db().display()));
        ForegroundServer::start(cmd, "hya server listening on ", Some(&disc))
    }
}

async fn server_sessions(url: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let body: Value = reqwest::Client::new()
        .get(format!("{url}/v1/sessions?includeArchived=true"))
        .send()
        .await?
        .json()
        .await?;
    Ok(body["sessions"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|r| r["parent"].as_str().is_none_or(str::is_empty))
                .filter_map(|r| r["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

#[tokio::test]
async fn routed_exec_unroutable_exit_75_and_readonly_commands_against_live_server() -> TestResult {
    let s = WriterScratch::new("hya-writers-exec")?;
    let server = s.start_server()?;

    for (args, needle) in [
        (&["--pure", "exec", "pure"][..], "hya serve stop"),
        (
            &["exec", "--time-budget", "30", "clock"][..],
            "--time-budget cannot apply to a running server",
        ),
        (
            &["exec", "--json", "--include-subagents", "tree"][..],
            "--include-subagents",
        ),
    ] {
        let out = s.run(args)?;
        let err = String::from_utf8(out.stderr)?;
        assert_eq!(out.status.code(), Some(EXIT_DB_IN_USE), "{err}");
        assert!(err.contains(&server.url) && err.contains(needle), "{err}");
    }
    assert!(server_sessions(&server.url).await?.is_empty());

    let plain = s.run(&["exec", "hello routed exec"])?;
    assert!(
        plain.status.success() && String::from_utf8(plain.stdout)?.contains("hello routed exec")
    );
    let first = server_sessions(&server.url).await?;
    assert_eq!(first.len(), 1);

    let json_out = s.run(&["exec", "--json", "hello routed json"])?;
    assert_success("routed json", &json_out);
    let sessions = server_sessions(&server.url).await?;
    assert_eq!(sessions.len(), 2);
    let second = sessions
        .iter()
        .find(|id| !first.contains(id))
        .ok_or("no second")?;
    let lines: Vec<Value> = String::from_utf8(json_out.stdout)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert!(lines.iter().all(|l| l["seq"] != 0));
    assert!(
        lines.iter().any(|l| l.to_string().contains(second))
            && lines
                .iter()
                .any(|l| l.to_string().contains("hello routed json"))
    );

    let tail = s.run(&["tail-session", &first[0]])?;
    assert!(tail.status.success() && String::from_utf8(tail.stdout)?.contains("hello routed exec"));
    let mem = s.hya().args(["exec", "in memory"]).output()?;
    assert!(mem.status.success() && String::from_utf8(mem.stdout)?.contains("in memory"));
    assert!(s.run(&["workflow", "list"])?.status.success());
    assert_eq!(server_sessions(&server.url).await?.len(), 2);
    assert_eq!(
        std::fs::read_to_string(s.lock())?.trim(),
        server.child.id().to_string()
    );
    Ok(())
}

#[tokio::test]
async fn workflow_use_run_state_and_sessions_archive_route_through_server() -> TestResult {
    let s = WriterScratch::new("hya-writers-wf")?;
    let server = s.start_server()?;

    let run = s.run(&["workflow", "run", "echo-input", "--input", "v=routed"])?;
    assert!(
        run.status.success()
            && String::from_utf8(run.stdout)?.contains("workflow echo-input: completed")
    );
    let sessions = server_sessions(&server.url).await?;
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];

    assert_eq!(
        s.run_json(
            "use",
            &[
                "workflow",
                "use",
                "echo-input",
                "--session",
                session,
                "--json"
            ]
        )?["state"]["selection"]["name"],
        "echo-input"
    );
    assert_eq!(
        s.run_json(
            "run2",
            &[
                "workflow",
                "run",
                "--session",
                session,
                "--input",
                "v=again",
                "--json"
            ]
        )?["result"]["run"]["status"],
        "completed"
    );
    assert_eq!(
        s.run_json(
            "state",
            &["workflow", "state", "--session", session, "--json"]
        )?["state"]["run"]["status"],
        "completed"
    );

    for (action, archived) in [("archive", true), ("unarchive", false)] {
        assert!(s.run(&["sessions", action, session])?.status.success());
        assert_eq!(
            hya_sdk_v1::V1Sdk::new(&server.url, "")
                .get_session(session)
                .await?
                .archived,
            archived
        );
    }
    assert_eq!(
        std::fs::read_to_string(s.lock())?.trim(),
        server.child.id().to_string()
    );

    let bad = s.run(&[
        "workflow",
        "run",
        "echo-input",
        "--input",
        "v=x",
        "--revision",
        &"0".repeat(64),
    ])?;
    assert_eq!(
        bad.status.code(),
        Some(EXIT_DB_IN_USE),
        "{}",
        combined_text(&bad)
    );
    assert_eq!(server_sessions(&server.url).await?.len(), 1);
    Ok(())
}

#[test]
fn goal_and_loop_leave_no_temporary_databases_on_success_or_error() -> TestResult {
    let s = WriterScratch::new("hya-writers-mem")?;
    let tmp = s.0.0.join("tmp");
    std::fs::create_dir(&tmp)?;
    for (args, ok) in [
        (&["-p", "one goal", "--max-iterations", "1"][..], true),
        (&["--model", "missing/model", "-p", "fail"][..], false),
        (&["loop", "--target", "done", "--until", "true"][..], true),
        (
            &["loop", "--target", "broken", "--until", "exit 2"][..],
            false,
        ),
    ] {
        let out = s
            .hya()
            .env("TMPDIR", &tmp)
            .arg("--db")
            .arg(s.db())
            .args(args)
            .output()?;
        assert_eq!(
            out.status.success(),
            ok,
            "{args:?}: {}",
            combined_text(&out)
        );
        assert!(
            !s.db().exists() && std::fs::read_dir(&tmp)?.next().is_none(),
            "{args:?} leaked files"
        );
    }
    Ok(())
}

#[test]
fn direct_writers_clear_metadata_after_run_and_hold_lock_while_active() -> TestResult {
    let s = WriterScratch::new("hya-writers-direct")?;
    assert!(s.run(&["exec", "direct"])?.status.success());
    assert!(std::fs::read_to_string(s.lock())?.trim().is_empty());

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let provider = format!("http://{}/v1", listener.local_addr()?);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for mut stream in listener.incoming().map_while(Result::ok) {
            let _ = stream.read(&mut [0u8; 4096]);
            let _ = tx.send(());
            held.push(stream);
        }
    });
    std::fs::create_dir_all(s.0.0.join("config/hya"))?;
    std::fs::write(
        s.0.0.join("config/hya/config.yaml"),
        format!(
            "default_model: stall/m\nproviders:\n  stall:\n    kind: openai-completion\n    base_url: {provider}\n    api_key: test\n    models: [m]\n"
        ),
    )?;
    let mut exec = s
        .hya()
        .arg("--db")
        .arg(s.db())
        .args(["exec", "never answered"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| -> TestResult {
        rx.recv_timeout(Duration::from_secs(15))
            .map_err(|_| "exec never reached provider")?;
        let pid = exec.id().to_string();
        let srv = s
            .hya()
            .args(["serve", "--bind", "127.0.0.1:0", "--db"])
            .arg(s.db())
            .output()?;
        assert_eq!(srv.status.code(), Some(EXIT_DB_IN_USE));
        assert!(String::from_utf8(srv.stderr)?.contains(&pid));
        let sid = hya_proto::SessionId::new().to_string();
        for args in [
            &["exec", "second"][..],
            &["workflow", "run", "echo-input", "--input", "v=x"][..],
            &["sessions", "archive", &sid][..],
        ] {
            let out = s.run(args)?;
            let err = String::from_utf8(out.stderr)?;
            assert!(
                out.status.code() == Some(EXIT_DB_IN_USE)
                    && err.contains("does not serve HTTP yet")
                    && err.contains(&pid),
                "{err}"
            );
        }
        Ok(())
    })();
    let _ = Command::new("kill")
        .args(["-INT", &exec.id().to_string()])
        .status();
    let deadline = Instant::now() + Duration::from_secs(10);
    while exec.try_wait()?.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = exec.kill();
    let _ = exec.wait();
    result
}
