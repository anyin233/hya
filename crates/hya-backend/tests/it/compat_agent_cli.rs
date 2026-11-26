//! Real-binary CLI integration tests for `exec`, `sessions`, `tail-session`,
//! `agent`, `models`, and `update`. Flag parsing (`--time-budget`, `--db`
//! precedence) and catalog rendering live in `src/` unit tests.

use serde_json::Value;

use crate::common::{Scratch, TestResult, assert_success};

fn session_created_field(
    output: &str,
    field: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line)?;
        if value.pointer("/event/type") == Some(&Value::String("session_created".into())) {
            return Ok(value
                .pointer(&format!("/event/{field}"))
                .and_then(Value::as_str)
                .map(str::to_owned));
        }
    }
    Ok(None)
}

fn write_config_with_default_agent(scratch: &Scratch, agent_id: &str) -> TestResult {
    let dir = scratch.0.join("config/hya");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("config.yaml"),
        format!(
            "default_model: hya/offline\nproviders: {{}}\nmcp: {{}}\nplugins: {{}}\npermission:\n  model: default\n  rules: []\ndefault_agent: {agent_id}\n"
        ),
    )?;
    Ok(())
}

#[test]
fn non_runtime_commands_skip_config_and_models_bootstraps_default_config() -> TestResult {
    let scratch = Scratch::new("hya-non-runtime")?;
    let cfg_dir = scratch.0.join("config/hya");

    let agents = scratch.hya().args(["agent", "list"]).output()?;
    assert_success("agent list", &agents);
    assert!(String::from_utf8(agents.stdout)?.contains("hya-main ("));

    let version = scratch.hya().args(["update", "version"]).output()?;
    assert_success("update version", &version);
    assert!(String::from_utf8(version.stdout)?.starts_with("hya update "));
    assert!(
        !cfg_dir.exists(),
        "non-runtime commands must not bootstrap config"
    );

    let models = scratch.hya().arg("models").output()?;
    assert_success("models", &models);
    let config = std::fs::read_to_string(cfg_dir.join("config.yaml"))?;
    assert!(config.contains("default_model: hya/offline") && config.contains("providers: {}"));
    Ok(())
}

#[test]
fn exec_db_persists_workdir_time_budget_and_tail_replays_selected_session() -> TestResult {
    let scratch = Scratch::new("hya-exec-db")?;
    let db_a = scratch.0.join("a.db");
    let db_b = scratch.0.join("b.db");

    let first = scratch
        .hya()
        .arg("--db")
        .arg(&db_a)
        .args([
            "exec",
            "--json",
            "--time-budget",
            "30",
            "budgeted offline prompt",
        ])
        .output()?;
    assert_success("exec --json --time-budget", &first);
    let first_out = String::from_utf8(first.stdout)?;
    assert!(
        first_out.contains("HEADLESS RUN — time budget: 30s"),
        "{first_out}"
    );
    assert!(first_out.contains("budgeted offline prompt"));
    let first_id = session_created_field(&first_out, "session")?.ok_or("missing session id")?;
    assert!(
        first_id.starts_with("hysec_") && first_id.len() == 26,
        "{first_id}"
    );
    let workdir = session_created_field(&first_out, "workdir")?.ok_or("missing workdir")?;
    assert_eq!(std::fs::canonicalize(&workdir)?, scratch.0);

    let second = scratch
        .hya()
        .arg("--db")
        .arg(&db_a)
        .args(["exec", "--json", "second in a"])
        .output()?;
    assert_success("second exec", &second);
    let second_id = session_created_field(&String::from_utf8(second.stdout)?, "session")?
        .ok_or("missing second id")?;

    let b_exec = scratch
        .hya()
        .arg("--db")
        .arg(&db_b)
        .args(["exec", "--json", "session in b"])
        .output()?;
    assert_success("exec db_b", &b_exec);
    let b_id = session_created_field(&String::from_utf8(b_exec.stdout)?, "session")?
        .ok_or("missing b id")?;

    // Subcommand --db overrides global --db for sessions and tail-session.
    let list_b = scratch
        .hya()
        .arg("--db")
        .arg(&db_a)
        .args(["sessions", "--db"])
        .arg(&db_b)
        .output()?;
    assert_success("sessions override", &list_b);
    let list_b_out = String::from_utf8(list_b.stdout)?;
    assert!(
        list_b_out.contains(&b_id) && !list_b_out.contains(&first_id),
        "{list_b_out}"
    );

    let tail = scratch
        .hya()
        .arg("--db")
        .arg(&db_b)
        .arg("tail-session")
        .arg(&second_id)
        .arg("--db")
        .arg(&db_a)
        .output()?;
    assert_success("tail-session", &tail);
    let tail_out = String::from_utf8(tail.stdout)?;
    let mut saw_created = false;
    for line in tail_out.lines().filter(|l| !l.trim().is_empty()) {
        let value: Value = serde_json::from_str(line)?;
        if let Some(id) = value.pointer("/event/session").and_then(Value::as_str) {
            assert_eq!(id, second_id, "replayed wrong session");
        }
        if value.pointer("/event/type") == Some(&Value::String("session_created".into())) {
            saw_created = true;
        }
    }
    assert!(
        saw_created,
        "tail-session missed session_created:\n{tail_out}"
    );
    Ok(())
}

#[test]
fn exec_resolves_project_bundle_default_agent_and_rejects_unselectable_agent() -> TestResult {
    let scratch = Scratch::new("hya-exec-agent")?;
    let bundle_dir = scratch.0.join(".hya/bundles/demo");
    std::fs::create_dir_all(bundle_dir.join("prompts"))?;
    std::fs::write(
        bundle_dir.join("bundle.yaml"),
        "kind: Bundle\nidentity:\n  id: hya/exec-project-demo\n  version: 1.0.0\n  publisher: hya\nagents:\n  - id: exec-project-lead\n    role: main\n    prompt: prompts/lead.md\n",
    )?;
    std::fs::write(
        bundle_dir.join("prompts/lead.md"),
        "You are the project lead.\n",
    )?;
    write_config_with_default_agent(&scratch, "exec-project-lead")?;

    let ok = scratch.hya().args(["exec", "--json", "hello"]).output()?;
    assert_success("exec project bundle agent", &ok);
    let out = String::from_utf8(ok.stdout)?;
    assert_eq!(
        session_created_field(&out, "agent")?.as_deref(),
        Some("exec-project-lead")
    );
    assert!(
        session_created_field(&out, "project")?.is_some_and(|p| !p.is_empty()),
        "missing project id:\n{out}"
    );

    write_config_with_default_agent(&scratch, "not-a-real-agent")?;
    let bad = scratch.hya().args(["exec", "--json", "hello"]).output()?;
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("not-a-real-agent"));
    Ok(())
}
