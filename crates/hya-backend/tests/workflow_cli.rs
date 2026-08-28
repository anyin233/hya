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

/// Fan-out level of two branches joined by one downstream stage; both branches
/// run against builtin agents the offline `build` caller may spawn.
const FAN_JOIN_YAML: &str = r#"
name: fan-join
description: two parallel branches joined into one merge stage
inputs:
  topic: subject every branch studies
stages:
  - id: branch-a
    agent: explore
    prompt: alpha branch on {{inputs.topic}}
  - id: branch-b
    agent: general
    prompt: beta branch on {{inputs.topic}}
  - id: merge
    agent: general
    needs: [branch-a, branch-b]
    prompt: "MERGED REPORT\n{{branch-a}}\n{{branch-b}}\nEND MERGE"
"#;

const SINGLE_INPUT_YAML: &str = r#"
name: echo-input
description: one stage whose directive embeds {{inputs.v}}
inputs:
  v: any value, equals signs included
stages:
  - id: capture
    agent: general
    prompt: captured={{inputs.v}}
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
fn cli_run_fans_out_two_branches_and_the_join_sees_both_sections()
-> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("fan-join")?;
    env.write_workflow("fan-join.yaml", FAN_JOIN_YAML)?;

    let output = workflow_command(&env)
        .args(["workflow", "run", "fan-join", "--input", "topic=engines"])
        .output()?;
    assert!(
        output.status.success(),
        "workflow run must exit 0: {}",
        combined_text(&output)
    );
    let stdout = String::from_utf8(output.stdout)?;

    // Deterministic per-stage report: overall status and one line per stage.
    assert!(
        stdout.contains("workflow fan-join: completed"),
        "missing overall status header:\n{stdout}"
    );
    assert!(
        stdout.contains("\n  branch-a [done] agent=explore session="),
        "branch-a stage row missing:\n{stdout}"
    );
    assert!(
        stdout.contains("\n  branch-b [done] agent=general session="),
        "branch-b stage row missing:\n{stdout}"
    );
    assert!(
        stdout.contains("\n  merge [done] agent=general session="),
        "merge stage row missing:\n{stdout}"
    );

    // The join's echoed directive is the only place these section headers can
    // appear, so their presence proves BOTH branches converged into the merge.
    assert!(
        stdout.contains("## upstream stage `branch-a` (explore)"),
        "merge directive missing branch-a section:\n{stdout}"
    );
    assert!(
        stdout.contains("## upstream stage `branch-b` (general)"),
        "merge directive missing branch-b section:\n{stdout}"
    );
    // And both branch bodies traveled through the join, not just headers.
    assert!(
        stdout.contains("alpha branch on engines"),
        "branch-a output text missing from the report:\n{stdout}"
    );
    assert!(
        stdout.contains("beta branch on engines"),
        "branch-b output text missing from the report:\n{stdout}"
    );
    Ok(())
}

#[test]
fn cli_rejects_missing_declared_input_before_any_spawn() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("missing-input")?;
    env.write_workflow("echo-input.yaml", SINGLE_INPUT_YAML)?;

    let output = workflow_command(&env)
        .args(["workflow", "run", "echo-input"])
        .output()?;
    assert!(
        !output.status.success(),
        "run without the declared input must fail:\n{}",
        combined_text(&output)
    );
    // The CLI-side guard fires before engine construction/spawn; its exact
    // wording distinguishes it from the in-core execution-time check.
    let text = combined_text(&output);
    assert!(
        text.contains("requires --input v=<value>"),
        "missing-input error not reported with CLI guidance:\n{text}"
    );
    Ok(())
}

#[test]
fn cli_parses_input_values_containing_equals_signs() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("equals-value")?;
    env.write_workflow("echo-input.yaml", SINGLE_INPUT_YAML)?;

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

    // JSON report: only the part before the FIRST '=' is the key; everything
    // after stays verbatim in the value the member received and echoed back.
    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("captured=a=b=c&d=e"),
        "member directive must carry the full value after the first '=' :\n{stdout}"
    );
    Ok(())
}

#[test]
fn cli_rejects_undeclared_input_keys_before_any_spawn() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("unknown-input")?;
    env.write_workflow("echo-input.yaml", SINGLE_INPUT_YAML)?;

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
        text.contains("does not declare input `ghost`"),
        "undeclared input error unclear:\n{text}"
    );
    assert!(
        text.contains("declared inputs: v"),
        "error should name the declared inputs:\n{text}"
    );
    Ok(())
}
