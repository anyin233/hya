//! Integration tests for `hya-backend workflow run`: the CLI run path.
//!
//! These tests execute REAL `workflow run` processes against the offline dev
//! provider. The dev provider echoes each member's directive back as its final
//! assistant text, so a joining stage's report proves exactly which upstream
//! sections were rendered into its directive — the end-to-end fan-in contract.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_bundle::{BundleSource, write_public_package};

/// Fan-out level of two branches joined by one downstream Stage.
const FAN_JOIN_WORKFLOW: &str = r#"---
kind: Workflow
name: fan-join
description: Two parallel branches joined into one merge Stage.
inputs:
  topic: Subject every branch studies.
nodes:
  branch-a:
    agent: explore
    directive: alpha branch on {{input.topic}}
  branch-b:
    agent: general
    directive: beta branch on {{input.topic}}
  merge:
    agent: general
    directive: MERGED REPORT
---
flowchart TD
  branch-a & branch-b --> merge
"#;

const SINGLE_INPUT_WORKFLOW: &str = r#"---
kind: Workflow
name: echo-input
description: One Stage whose directive embeds the required input.
inputs:
  v: Any value, including equals signs.
nodes:
  capture:
    agent: general
    directive: captured={{input.v}}
---
flowchart TD
  capture
"#;

const ROUTED_WORKFLOW: &str = r#"---
kind: Workflow
name: routed
description: One Stage with an explicit offline model route.
nodes:
  execute:
    agent: general
    directive: Execute through the routed model.
    model:
      id: offline
---
flowchart TD
  execute
"#;

struct IsolatedEnv {
    root: PathBuf,
    home: PathBuf,
    xdg_config: PathBuf,
    hya_config: PathBuf,
    workdir: PathBuf,
    path: Option<OsString>,
}

impl IsolatedEnv {
    /// Fully isolated HOME/config/workdir so runs never touch developer state.
    fn new(prefix: &str) -> Result<Self, Box<dyn std::error::Error>> {
        static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let serial = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "hya-backend-workflow-cli-{prefix}-{}-{nanos}-{serial}",
            std::process::id()
        ));
        let home = root.join("home");
        let xdg_config = root.join("xdg-config");
        let hya_config = root.join("hya-config");
        let workdir = root.join("workdir");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&xdg_config)?;
        std::fs::create_dir_all(&hya_config)?;
        std::fs::create_dir_all(workdir.join(".hya").join("workflows"))?;
        Ok(Self {
            root,
            home,
            xdg_config,
            hya_config,
            workdir,
            path: std::env::var_os("PATH"),
        })
    }

    fn write_workflow(&self, file_name: &str, body: &str) -> Result<(), std::io::Error> {
        fs::write(
            self.workdir.join(".hya").join("workflows").join(file_name),
            body,
        )
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Return the repository root that owns the shipped WorkflowBundle examples.
fn repository_root() -> Result<PathBuf, std::io::Error> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| std::io::Error::other("hya-backend must live under <repository>/crates"))
}

fn workflow_command(env: &IsolatedEnv) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya-backend"));
    command.env_clear();
    if let Some(path) = &env.path {
        command.env("PATH", path);
    }
    command
        .env("HOME", &env.home)
        .env("XDG_CONFIG_HOME", &env.xdg_config)
        .env("HYA_CONFIG_HOME", &env.hya_config)
        .env("NO_COLOR", "1")
        .current_dir(&env.workdir);
    command
}

fn combined_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn cli_run_reports_durable_fan_out_and_join_projection() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("fan-join")?;
    env.write_workflow("fan-join.hya.md", FAN_JOIN_WORKFLOW)?;

    let output = workflow_command(&env)
        .args(["workflow", "run", "fan-join", "--input", "topic=engines"])
        .output()?;
    assert!(
        output.status.success(),
        "workflow run must exit 0: {}",
        combined_text(&output)
    );
    let stdout = String::from_utf8(output.stdout)?;

    assert!(
        stdout.contains("workflow fan-join: completed"),
        "missing overall status header:\n{stdout}"
    );
    assert!(
        stdout.contains("\n  branch-a [completed] agent=explore members=1"),
        "branch-a Stage row missing:\n{stdout}"
    );
    assert!(
        stdout.contains("\n  branch-b [completed] agent=general members=1"),
        "branch-b Stage row missing:\n{stdout}"
    );
    assert!(
        stdout.contains("\n  merge [completed] agent=general members=1"),
        "merge Stage row missing:\n{stdout}"
    );
    Ok(())
}

#[test]
fn cli_info_and_run_print_requested_selected_and_outcome_routes()
-> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("routed-model")?;
    env.write_workflow("routed.hya.md", ROUTED_WORKFLOW)?;

    let info = workflow_command(&env)
        .args(["workflow", "info", "routed"])
        .output()?;
    assert!(
        info.status.success(),
        "workflow info must exit 0: {}",
        combined_text(&info)
    );
    let info_stdout = String::from_utf8(info.stdout)?;
    assert!(
        info_stdout.contains("worker model: offline reasoning=default"),
        "requested route missing from workflow info:\n{info_stdout}"
    );

    let run = workflow_command(&env)
        .args(["workflow", "run", "routed"])
        .output()?;
    assert!(
        run.status.success(),
        "model-routed workflow run must exit 0: {}",
        combined_text(&run)
    );
    let run_stdout = String::from_utf8(run.stdout)?;
    for expected in [
        "worker model: offline reasoning=default",
        "selected worker model: #0 offline reasoning=none",
        "route outcome: role=worker iteration=0 step=0 candidate=#0 model=offline reasoning=none class=none",
    ] {
        assert!(
            run_stdout.contains(expected),
            "model-routed workflow output missing `{expected}`:\n{run_stdout}"
        );
    }
    assert!(!run_stdout.contains("provider response"));
    Ok(())
}

#[test]
fn cli_rejects_missing_declared_input_before_any_spawn() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("missing-input")?;
    env.write_workflow("echo-input.hya.md", SINGLE_INPUT_WORKFLOW)?;

    let output = workflow_command(&env)
        .args(["workflow", "run", "echo-input"])
        .output()?;
    assert!(
        !output.status.success(),
        "run without the declared input must fail:\n{}",
        combined_text(&output)
    );
    let text = combined_text(&output);
    assert!(
        text.contains("required Workflow input `v` was not provided"),
        "missing-input error not reported clearly:\n{text}"
    );
    Ok(())
}

#[test]
fn cli_parses_input_values_containing_equals_signs() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("equals-value")?;
    env.write_workflow("echo-input.hya.md", SINGLE_INPUT_WORKFLOW)?;

    let output = workflow_command(&env)
        .args([
            "workflow",
            "run",
            "echo-input",
            "--input",
            "v=a=b=c&d=e",
            "--json",
        ])
        .output()?;
    assert!(
        output.status.success(),
        "k=v parsing with '=' inside the value must succeed: {}",
        combined_text(&output)
    );

    // The shared durable result proves the CLI accepted the value and completed
    // without echoing raw input values into lifecycle state.
    let stdout = String::from_utf8(output.stdout)?;
    let value: serde_json::Value = serde_json::from_str(&stdout)?;
    assert_eq!(value["kind"], "run");
    assert_eq!(value["result"]["run"]["status"], "completed");
    assert_eq!(value["result"]["run"]["stages"][0]["status"], "completed");
    assert!(
        !stdout.contains("a=b=c&d=e"),
        "raw Workflow inputs must not enter durable result state: {stdout}"
    );
    Ok(())
}

#[test]
fn cli_rejects_undeclared_input_keys_before_any_spawn() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("unknown-input")?;
    env.write_workflow("echo-input.hya.md", SINGLE_INPUT_WORKFLOW)?;

    let output = workflow_command(&env)
        .args([
            "workflow",
            "run",
            "echo-input",
            "--input",
            "v=ok",
            "--input",
            "ghost=surprise",
        ])
        .output()?;
    assert!(
        !output.status.success(),
        "undeclared input key must fail the run:\n{}",
        combined_text(&output)
    );
    let text = combined_text(&output);
    assert!(
        text.contains("Workflow input `ghost` is not declared"),
        "undeclared input error unclear:\n{text}"
    );
    Ok(())
}

/// The shipped Argus source packages, installs, resolves, and runs through public CLI paths.
#[test]
fn cli_installs_and_runs_shipped_argus_workflowbundle() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("argus-example")?;
    let source =
        BundleSource::read_directory(repository_root()?.join("bundles/examples/argus-example"))?;
    let package = env.root.join("hya-argus-example.hyabundle");
    fs::write(&package, write_public_package(&source)?)?;

    let installed = workflow_command(&env)
        .args(["bundle", "install"])
        .arg(&package)
        .output()?;
    assert!(
        installed.status.success(),
        "Argus package install must succeed: {}",
        combined_text(&installed)
    );

    let run = workflow_command(&env)
        .args([
            "workflow",
            "run",
            "argus",
            "--input",
            "request=verify-the-shipped-example",
            "--json",
        ])
        .output()?;
    assert!(
        run.status.success(),
        "installed Argus Workflow must complete: {}",
        combined_text(&run)
    );
    let result: serde_json::Value = serde_json::from_slice(&run.stdout)?;
    assert_eq!(result["kind"], "run");
    assert_eq!(result["result"]["run"]["workflow"]["name"], "argus");
    assert_eq!(result["result"]["run"]["status"], "completed");
    assert_eq!(
        result["result"]["run"]["stages"].as_array().map(Vec::len),
        Some(10)
    );
    Ok(())
}

/// Selection without an owning Session must fail instead of creating unreachable state.
#[test]
fn cli_use_requires_an_existing_session() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("use-requires-session")?;
    env.write_workflow("echo-input.hya.md", SINGLE_INPUT_WORKFLOW)?;

    let output = workflow_command(&env)
        .args(["workflow", "use", "echo-input"])
        .output()?;
    assert!(
        !output.status.success(),
        "workflow use without --session must fail:\n{}",
        combined_text(&output)
    );
    assert!(
        combined_text(&output).contains("--session"),
        "missing Session error must name --session: {}",
        combined_text(&output)
    );
    Ok(())
}

/// Read-only catalog commands must not create durable Session rows, and
/// duplicate input keys must fail before an owning Session is created.
#[test]
fn cli_read_only_commands_and_duplicate_inputs_leave_the_database_unchanged()
-> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("read-only-and-duplicate-inputs")?;
    env.write_workflow("echo-input.hya.md", SINGLE_INPUT_WORKFLOW)?;
    let db = env.root.join("sessions.db");

    for args in [
        vec!["workflow", "list"],
        vec!["workflow", "info", "echo-input"],
    ] {
        let output = workflow_command(&env)
            .arg("--db")
            .arg(&db)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "read-only Workflow command failed: {}",
            combined_text(&output)
        );
    }
    let duplicate = workflow_command(&env)
        .arg("--db")
        .arg(&db)
        .args([
            "workflow",
            "run",
            "echo-input",
            "--input",
            "v=first",
            "--input",
            "v=second",
        ])
        .output()?;
    assert!(
        !duplicate.status.success(),
        "duplicate Workflow input was accepted"
    );
    assert!(combined_text(&duplicate).contains("duplicate --input key `v`"));

    let sessions = workflow_command(&env)
        .args(["sessions", "--db"])
        .arg(&db)
        .output()?;
    assert!(String::from_utf8(sessions.stdout)?.contains("no sessions found"));
    Ok(())
}

/// A nameless run has no durable selection unless an owning Session is supplied.
#[test]
fn cli_run_without_name_or_session_fails_before_creating_state()
-> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("run-requires-name-or-session")?;
    env.write_workflow("echo-input.hya.md", SINGLE_INPUT_WORKFLOW)?;
    let db = env.root.join("sessions.db");

    let output = workflow_command(&env)
        .arg("--db")
        .arg(&db)
        .args(["workflow", "run", "--input", "v=value"])
        .output()?;
    assert!(
        !output.status.success(),
        "nameless Workflow run without --session must fail:\n{}",
        combined_text(&output)
    );
    assert!(
        combined_text(&output).contains("requires NAME unless --session"),
        "missing binding error was unclear: {}",
        combined_text(&output)
    );
    let sessions = workflow_command(&env)
        .args(["sessions", "--db"])
        .arg(&db)
        .output()?;
    assert!(
        String::from_utf8(sessions.stdout)?.contains("no sessions found"),
        "rejected nameless run created durable Session state"
    );
    Ok(())
}
/// Separate CLI processes must select and read Workflow state in one durable Session.
#[test]
fn cli_use_and_state_share_an_existing_session_across_processes()
-> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("durable-session")?;
    env.write_workflow("echo-input.hya.md", SINGLE_INPUT_WORKFLOW)?;
    env.write_workflow("fan-join.hya.md", FAN_JOIN_WORKFLOW)?;
    let db = env.root.join("sessions.db");

    let initial = workflow_command(&env)
        .arg("--db")
        .arg(&db)
        .args(["workflow", "run", "echo-input", "--input", "v=seed"])
        .output()?;
    assert!(
        initial.status.success(),
        "initial Workflow run must create a durable Session: {}",
        combined_text(&initial)
    );
    let sessions = workflow_command(&env)
        .args(["sessions", "--db"])
        .arg(&db)
        .output()?;
    assert!(
        sessions.status.success(),
        "Session listing must succeed: {}",
        combined_text(&sessions)
    );
    let sessions_stdout = String::from_utf8(sessions.stdout)?;
    let session = sessions_stdout
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .ok_or("durable Workflow run did not create a listed Session")?;

    let selected = workflow_command(&env)
        .arg("--db")
        .arg(&db)
        .args([
            "workflow",
            "use",
            "fan-join",
            "--session",
            session,
            "--json",
        ])
        .output()?;
    assert!(
        selected.status.success(),
        "Workflow selection must bind to the existing Session: {}",
        combined_text(&selected)
    );
    let selected_json: serde_json::Value = serde_json::from_slice(&selected.stdout)?;
    assert_eq!(selected_json["state"]["selection"]["name"], "fan-join");

    let selected_run = workflow_command(&env)
        .arg("--db")
        .arg(&db)
        .args([
            "workflow",
            "run",
            "--session",
            session,
            "--input",
            "topic=durable",
            "--json",
        ])
        .output()?;
    assert!(
        selected_run.status.success(),
        "Workflow run must use the existing Session selection: {}",
        combined_text(&selected_run)
    );
    let selected_run_json: serde_json::Value = serde_json::from_slice(&selected_run.stdout)?;
    assert_eq!(
        selected_run_json["result"]["run"]["workflow"]["name"],
        "fan-join"
    );
    assert_eq!(selected_run_json["result"]["run"]["status"], "completed");

    let state = workflow_command(&env)
        .arg("--db")
        .arg(&db)
        .args(["workflow", "state", "--session", session, "--json"])
        .output()?;
    assert!(
        state.status.success(),
        "separate Workflow state process must replay the Session: {}",
        combined_text(&state)
    );
    let state_json: serde_json::Value = serde_json::from_slice(&state.stdout)?;
    assert_eq!(state_json["kind"], "state");
    assert_eq!(state_json["state"]["selection"]["name"], "fan-join");
    assert_eq!(state_json["state"]["run"]["workflow"]["name"], "fan-join");
    Ok(())
}
