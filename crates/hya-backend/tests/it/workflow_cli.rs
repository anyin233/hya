//! Real-binary integration tests for `hya workflow`: offline fan-in run,
//! cross-process durable Session handoff, and the shipped Argus WorkflowBundle.
//! Pure input/revision parsing, output formatting, and read-only catalog scope
//! resolution live in `src/workflow_cmd.rs` unit tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use hya_bundle::{BundleSource, write_public_package};
use serde_json::Value;

use crate::common::{Scratch, TestResult, assert_success, combined_text};

const FAN_JOIN_WF: &str = "---\nkind: Workflow\nname: fan-join\ndescription: Two parallel branches joined.\ninputs:\n  topic: Subject.\nnodes:\n  branch-a: { agent: hya-scout, directive: 'alpha {{input.topic}}' }\n  branch-b: { agent: hya-task, directive: 'beta {{input.topic}}' }\n  merge: { agent: hya-task, directive: MERGED REPORT }\n---\nflowchart TD\n  branch-a & branch-b --> merge\n";
const ECHO_WF: &str = "---\nkind: Workflow\nname: echo-input\ndescription: Echo.\ninputs:\n  v: Any value.\nnodes:\n  capture: { agent: hya-task, directive: 'captured={{input.v}}' }\n---\nflowchart TD\n  capture\n";
const ROUTED_WF: &str = "---\nkind: Workflow\nname: routed\ndescription: Routed.\nnodes:\n  execute: { agent: hya-task, directive: Execute, model: { id: hya/offline } }\n---\nflowchart TD\n  execute\n";

fn write_wf(s: &Scratch, file: &str, body: &str) -> Result<(), std::io::Error> {
    let dir = s.0.join(".hya/workflows");
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(file), body)
}

fn wf(s: &Scratch, db: Option<&Path>, args: &[&str]) -> Result<Output, std::io::Error> {
    let mut cmd = s.hya();
    if let Some(db) = db {
        cmd.arg("--db").arg(db);
    }
    cmd.arg("workflow").args(args).output()
}

fn wf_json(
    label: &str,
    s: &Scratch,
    db: Option<&Path>,
    args: &[&str],
) -> Result<Value, Box<dyn std::error::Error>> {
    let out = wf(s, db, args)?;
    assert_success(label, &out);
    Ok(serde_json::from_slice(&out.stdout)?)
}

#[test]
fn cli_run_fan_join_routed_models_and_read_only_catalog() -> TestResult {
    let s = Scratch::new("hya-wf-run")?;
    write_wf(&s, "fan-join.hya.md", FAN_JOIN_WF)?;
    write_wf(&s, "routed.hya.md", ROUTED_WF)?;
    let db = s.0.join("sessions.db");

    // Malformed runtime config must not affect read-only catalog inspection or create a DB.
    let cfg = s.0.join("config/hya");
    fs::create_dir_all(&cfg)?;
    fs::write(cfg.join("config.yaml"), "providers: [\n")?;
    let info = wf(&s, Some(&db), &["info", "routed"])?;
    assert_success("info routed", &info);
    assert!(
        String::from_utf8(info.stdout)?.contains("worker model: hya/offline reasoning=default")
            && info.stderr.is_empty()
            && !db.exists()
    );
    fs::remove_file(cfg.join("config.yaml"))?;

    let fan = wf(&s, None, &["run", "fan-join", "--input", "topic=engines"])?;
    assert_success("run fan-join", &fan);
    let fan_out = String::from_utf8(fan.stdout)?;
    for needle in [
        "workflow fan-join: completed",
        "\n  branch-a [completed] agent=hya-scout members=1",
        "\n  branch-b [completed] agent=hya-task members=1",
        "\n  merge [completed] agent=hya-task members=1",
    ] {
        assert!(fan_out.contains(needle), "missing {needle:?}:\n{fan_out}");
    }

    let routed = wf(&s, None, &["run", "routed"])?;
    assert_success("run routed", &routed);
    let r_out = String::from_utf8(routed.stdout)?;
    assert!(
        r_out.contains("selected worker model: #0 hya/offline reasoning=none")
            && r_out.contains("route outcome: role=worker iteration=0 step=0 candidate=#0 model=hya/offline reasoning=none class=none"),
        "{r_out}"
    );
    Ok(())
}

#[test]
fn cli_use_run_and_state_share_durable_session_and_reject_bad_inputs() -> TestResult {
    let s = Scratch::new("hya-wf-session")?;
    write_wf(&s, "echo-input.hya.md", ECHO_WF)?;
    write_wf(&s, "fan-join.hya.md", FAN_JOIN_WF)?;
    let db = s.0.join("sessions.db");

    let missing = wf(&s, Some(&db), &["run", "echo-input"])?;
    assert!(
        !missing.status.success()
            && combined_text(&missing).contains("required Workflow input `v` was not provided")
    );
    let unknown = wf(
        &s,
        Some(&db),
        &["run", "echo-input", "--input", "v=ok", "--input", "ghost=1"],
    )?;
    assert!(
        !unknown.status.success()
            && combined_text(&unknown).contains("Workflow input `ghost` is not declared")
    );

    let init = wf(
        &s,
        Some(&db),
        &["run", "echo-input", "--input", "v=a=b=c&d=e", "--json"],
    )?;
    assert_success("initial run", &init);
    assert!(!String::from_utf8(init.stdout)?.contains("a=b=c&d=e"));

    let sess_out = s.hya().args(["sessions", "--db"]).arg(&db).output()?;
    assert_success("sessions", &sess_out);
    let sess_text = String::from_utf8(sess_out.stdout)?;
    let session = sess_text
        .lines()
        .find(|l| l.starts_with("hysec_"))
        .and_then(|l| l.split_whitespace().next())
        .ok_or("missing root session")?;

    assert_eq!(
        wf_json(
            "use",
            &s,
            Some(&db),
            &["use", "fan-join", "--session", session, "--json"]
        )?["state"]["selection"]["name"],
        "fan-join"
    );
    let run = wf_json(
        "run",
        &s,
        Some(&db),
        &[
            "run",
            "--session",
            session,
            "--input",
            "topic=durable",
            "--json",
        ],
    )?;
    assert_eq!(
        (
            &run["result"]["run"]["workflow"]["name"],
            &run["result"]["run"]["status"]
        ),
        (&Value::from("fan-join"), &Value::from("completed"))
    );
    let st = wf_json(
        "state",
        &s,
        Some(&db),
        &["state", "--session", session, "--json"],
    )?;
    assert_eq!(
        (
            &st["state"]["selection"]["name"],
            &st["state"]["run"]["workflow"]["name"]
        ),
        (&Value::from("fan-join"), &Value::from("fan-join"))
    );
    Ok(())
}

#[test]
fn cli_installs_and_runs_shipped_argus_workflowbundle() -> TestResult {
    let s = Scratch::new("hya-wf-argus")?;
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("repo")?
        .to_path_buf();
    let pkg = s.0.join("hya-argus-example.hyabundle");
    fs::write(
        &pkg,
        write_public_package(&BundleSource::read_directory(
            repo.join("bundles/examples/argus-example"),
        )?)?,
    )?;
    assert_success(
        "install argus",
        &s.hya()
            .args(["bundle", "install", "-y"])
            .arg(&pkg)
            .output()?,
    );

    let info = wf_json("info argus", &s, None, &["info", "argus", "--json"])?;
    let res = wf_json(
        "run argus",
        &s,
        None,
        &[
            "run",
            "argus",
            "--input",
            "request=verify-shipped",
            "--json",
        ],
    )?;
    assert_eq!(
        res["result"]["run"]["workflow"],
        info["workflow"]["identity"]
    );
    assert_eq!(res["result"]["run"]["status"], "completed");
    assert_eq!(
        res["result"]["run"]["stages"].as_array().map(Vec::len),
        Some(10)
    );
    Ok(())
}

#[test]
fn cli_rejects_workflow_commands_in_bare_or_when_bundle_disabled() -> TestResult {
    let s = Scratch::new("hya-wf-disabled")?;
    let bare_out = s.hya().args(["--bare", "workflow", "list"]).output()?;
    assert!(
        !bare_out.status.success() && combined_text(&bare_out).contains("hya/workflow"),
        "expected actionable --bare error, got:\n{}",
        combined_text(&bare_out)
    );

    let cfg = s.0.join("config/hya");
    fs::create_dir_all(&cfg)?;
    fs::write(
        cfg.join("config.yaml"),
        "schema_version: 2\nbundles:\n  builtin:\n    profile: full\n    disable:\n      - hya/workflow\n",
    )?;
    let disabled_out = wf(&s, None, &["list"])?;
    assert!(
        !disabled_out.status.success() && combined_text(&disabled_out).contains("hya/workflow"),
        "expected actionable disabled-bundle error, got:\n{}",
        combined_text(&disabled_out)
    );
    Ok(())
}
