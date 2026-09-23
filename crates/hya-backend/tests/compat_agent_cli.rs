//! Integration tests for `hya`: compat agent cli.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_proto::{OperationId, SessionId, ToolCallId};
use hya_store::{AdmissionClaim, AdmissionState, SessionStore};
use serde_json::Value;

#[test]
fn agent_list_prints_compat_native_agent_shape() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-agent-list")?;
    let output = hya_command(&env).args(["agent", "list"]).output()?;

    assert_success("agent list", &output);
    assert_eq!(
        String::from_utf8(output.stdout)?,
        concat!(
            "build (primary)\n",
            "  [\n",
            "  {\n",
            "    \"permission\": \"read\",\n",
            "    \"pattern\": \"*\",\n",
            "    \"action\": \"allow\"\n",
            "  },\n",
            "  {\n",
            "    \"permission\": \"glob\",\n",
            "    \"pattern\": \"*\",\n",
            "    \"action\": \"allow\"\n",
            "  },\n",
            "  {\n",
            "    \"permission\": \"grep\",\n",
            "    \"pattern\": \"*\",\n",
            "    \"action\": \"allow\"\n",
            "  }\n",
            "]\n",
        )
    );
    Ok(())
}

#[test]
fn models_command_creates_default_config_when_missing() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-first-run-config")?;
    let path = env.xdg_config.join("hya/config.yaml");
    assert!(!path.exists(), "test should start without hya config");

    let output = hya_command(&env).arg("models").output()?;

    assert_success("models", &output);
    let config = std::fs::read_to_string(&path)?;
    assert!(
        config.contains("default_model: hya/offline"),
        "created config should contain the offline starter model:\n{config}"
    );
    assert!(
        config.contains("providers: {}"),
        "created config should contain an empty providers map:\n{config}"
    );
    Ok(())
}

#[test]
fn update_commands_run_without_composing_the_runtime() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-update-cli")?;
    let roots = env.root.join("updater/trust_roots.json");
    std::fs::create_dir_all(env.root.join("updater"))?;
    let key = format!("release={}", "ab".repeat(32));

    let init = hya_command(&env)
        .args(["update", "init-roots", "--path"])
        .arg(&roots)
        .args(["--root", &key])
        .output()?;
    assert_success("update init-roots", &init);
    assert!(roots.is_file(), "init-roots must write {}", roots.display());

    let version = hya_command(&env).args(["update", "version"]).output()?;
    assert_success("update version", &version);
    assert!(String::from_utf8(version.stdout)?.starts_with("hya update "));

    assert!(
        !env.xdg_config.join("hya").exists(),
        "`hya update` must not bootstrap runtime config"
    );
    Ok(())
}

#[test]
fn rendered_exec_db_persists_and_tail_replays_hysec_session()
-> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-rendered-exec-db")?;
    let db = env.root.join("hya.db");

    let exec = hya_command(&env)
        .args(["--db"])
        .arg(&db)
        .args(["exec", "Say exactly hysec"])
        .output()?;
    assert_success("exec --db", &exec);

    let sessions = hya_command(&env)
        .args(["sessions", "--db"])
        .arg(&db)
        .output()?;
    assert_success("sessions --db", &sessions);
    let sessions_stdout = String::from_utf8(sessions.stdout)?;
    let session = extract_hysec_id(&sessions_stdout).ok_or("missing hysec session id")?;
    assert!(is_hysec_id(&session), "invalid session id: {session}");
    assert_listed_session(&sessions_stdout, &session);

    let global_sessions = hya_command(&env)
        .args(["--db"])
        .arg(&db)
        .arg("sessions")
        .output()?;
    assert_success("--db sessions", &global_sessions);
    assert_listed_session(&String::from_utf8(global_sessions.stdout)?, &session);

    let tail = hya_command(&env)
        .args(["tail-session"])
        .arg(&session)
        .args(["--db"])
        .arg(&db)
        .output()?;
    assert_success("tail-session hysec --db", &tail);
    let tail_stdout = String::from_utf8(tail.stdout)?;
    assert_tail_replays_session(&tail_stdout, &session)?;

    let global_tail = hya_command(&env)
        .args(["--db"])
        .arg(&db)
        .args(["tail-session"])
        .arg(&session)
        .output()?;
    assert_success("--db tail-session hysec", &global_tail);
    assert_tail_replays_session(&String::from_utf8(global_tail.stdout)?, &session)?;

    Ok(())
}

#[test]
fn json_exec_db_emits_hysec_session_and_sessions_lists_exact_id()
-> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-json-exec-db")?;
    let db = env.root.join("json.db");

    let exec = hya_command(&env)
        .args(["--db"])
        .arg(&db)
        .args(["exec", "--json", "Say exactly hysec json"])
        .output()?;
    assert_success("exec --json --db", &exec);
    let exec_stdout = String::from_utf8(exec.stdout)?;
    let session = session_created_id(&exec_stdout)?.ok_or("missing session_created event")?;
    assert!(is_hysec_id(&session), "invalid session id: {session}");

    let sessions = hya_command(&env)
        .args(["sessions", "--db"])
        .arg(&db)
        .output()?;
    assert_success("sessions --db", &sessions);
    let sessions_stdout = String::from_utf8(sessions.stdout)?;
    assert_listed_session(&sessions_stdout, &session);

    Ok(())
}

#[test]
fn exec_starts_the_root_session_under_the_configured_default_agent()
-> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-exec-default-agent")?;
    write_config_with_default_agent(&env, "plan")?;

    let output = hya_command(&env)
        .args(["exec", "--json", "hello"])
        .output()?;
    assert_success("exec --json (default_agent: plan)", &output);

    let stdout = String::from_utf8(output.stdout)?;
    let agent = session_created_agent(&stdout)?.ok_or("missing session_created event")?;
    assert_eq!(
        agent, "plan",
        "hya exec must start the root session under the configured `default_agent`, \
         not the built-in `build` agent:\n{stdout}"
    );

    Ok(())
}

#[test]
fn run_honors_configured_default_agent_like_exec() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-run-default-agent")?;
    write_config_with_default_agent(&env, "hya-main")?;

    let output = hya_command(&env)
        .args(["run", "--json", "hello"])
        .output()?;
    assert_success("run --json (default_agent: hya-main)", &output);

    let stdout = String::from_utf8(output.stdout)?;
    let agent = session_created_agent(&stdout)?.ok_or("missing session_created event")?;
    assert_eq!(
        agent, "hya-main",
        "hya run (the exec alias) must honor `default_agent` the same way exec does:\n{stdout}"
    );

    Ok(())
}

#[test]
fn exec_fails_clearly_when_default_agent_is_unselectable() -> Result<(), Box<dyn std::error::Error>>
{
    let env = IsolatedEnv::new("hya-exec-unknown-default-agent")?;
    write_config_with_default_agent(&env, "not-a-real-agent")?;

    let output = hya_command(&env)
        .args(["exec", "--json", "hello"])
        .output()?;
    assert!(
        !output.status.success(),
        "exec should fail clearly on an unselectable default_agent instead of \
         silently falling back to `build`:\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("not-a-real-agent"),
        "expected the typed UnknownAgentId error to name the bad agent id:\n{stderr}"
    );

    Ok(())
}

#[test]
fn sessions_empty_db_prints_no_sessions_found() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-empty-sessions-db")?;
    let db = env.root.join("empty.db");
    let expected = format!("no sessions found in {}\n", db.display());

    let sessions = hya_command(&env)
        .args(["sessions", "--db"])
        .arg(&db)
        .output()?;
    assert_success("sessions --db", &sessions);
    assert_eq!(String::from_utf8(sessions.stdout)?, expected);

    let global_sessions = hya_command(&env)
        .args(["--db"])
        .arg(&db)
        .arg("sessions")
        .output()?;
    assert_success("--db sessions", &global_sessions);
    assert_eq!(String::from_utf8(global_sessions.stdout)?, expected);

    Ok(())
}

#[test]
fn tail_session_json_replays_only_requested_session_when_multiple_sessions_exist()
-> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-tail-selected-session")?;
    let db = env.root.join("tail.db");

    let first = exec_json_session(&env, &db, "first offline session")?;
    let second = exec_json_session(&env, &db, "second offline session")?;
    assert_ne!(first, second, "expected two distinct sessions");

    let sessions = hya_command(&env)
        .args(["sessions", "--db"])
        .arg(&db)
        .output()?;
    assert_success("sessions --db", &sessions);
    let sessions_stdout = String::from_utf8(sessions.stdout)?;
    assert_listed_session(&sessions_stdout, &first);
    assert_listed_session(&sessions_stdout, &second);

    let tail = hya_command(&env)
        .arg("tail-session")
        .arg(&second)
        .args(["--db"])
        .arg(&db)
        .output()?;
    assert_success("tail-session --db", &tail);
    assert_tail_replays_session(&String::from_utf8(tail.stdout)?, &second)?;

    Ok(())
}

#[test]
fn tail_session_does_not_run_spawn_admission_recovery() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-tail-admission-read-only")?;
    let db = env.root.join("tail-admission.db");
    let db_text = db.to_string_lossy().into_owned();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let root = SessionId::new();
    let source = ToolCallId::new();
    let operation = OperationId::from_tool_call(source);
    runtime.block_on(async {
        let store = SessionStore::connect(&db_text).await?;
        store
            .claim_admission(&AdmissionClaim {
                operation_id: operation,
                source_tool_call_id: source,
                root_session: root,
                request_fingerprint: [31; 32],
                admission_units: 1,
                actor_claim: None,
            })
            .await?;
        store.start_admission(operation, None).await?;
        Ok::<(), hya_store::StoreError>(())
    })?;

    let tail = hya_command(&env)
        .arg("tail-session")
        .arg(root.to_string())
        .args(["--db"])
        .arg(&db)
        .output()?;
    assert_success("tail-session must stay read-only", &tail);

    let state = runtime.block_on(async {
        SessionStore::connect(&db_text)
            .await?
            .admission(operation)
            .await
            .map(|record| record.map(|record| record.state))
    })?;
    assert_eq!(state, Some(AdmissionState::Started));
    Ok(())
}

#[test]
fn subcommand_db_overrides_global_db_for_sessions_and_tail()
-> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-subcommand-db-override")?;
    let db_a = env.root.join("a.db");
    let db_b = env.root.join("b.db");

    let session_a = exec_json_session(&env, &db_a, "session in db a")?;
    let session_b = exec_json_session(&env, &db_b, "session in db b")?;
    assert_ne!(
        session_a, session_b,
        "expected each database to contain its own session"
    );

    let sessions = hya_command(&env)
        .args(["--db"])
        .arg(&db_a)
        .args(["sessions", "--db"])
        .arg(&db_b)
        .output()?;
    assert_success("--db sessions --db", &sessions);
    let sessions_stdout = String::from_utf8(sessions.stdout)?;
    assert_listed_session(&sessions_stdout, &session_b);
    assert_session_not_listed(&sessions_stdout, &session_a);

    let tail = hya_command(&env)
        .args(["--db"])
        .arg(&db_a)
        .arg("tail-session")
        .arg(&session_b)
        .args(["--db"])
        .arg(&db_b)
        .output()?;
    assert_success("--db tail-session --db", &tail);
    assert_tail_replays_session(&String::from_utf8(tail.stdout)?, &session_b)?;

    Ok(())
}

struct IsolatedEnv {
    root: PathBuf,
    home: PathBuf,
    xdg_config: PathBuf,
    hya_config: PathBuf,
    workdir: PathBuf,
    path: Option<OsString>,
}

impl IsolatedEnv {
    fn new(prefix: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let root = temp_dir(prefix)?;
        let home = root.join("home");
        let xdg_config = root.join("xdg-config");
        let hya_config = root.join("hya-config");
        let workdir = root.join("workdir");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&xdg_config)?;
        std::fs::create_dir_all(&hya_config)?;
        std::fs::create_dir_all(&workdir)?;
        Ok(Self {
            root,
            home,
            xdg_config,
            hya_config,
            workdir,
            path: std::env::var_os("PATH"),
        })
    }
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn hya_command(env: &IsolatedEnv) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
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

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn temp_dir(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir)?;
    Ok(dir)
}

fn extract_hysec_id(text: &str) -> Option<String> {
    text.split(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
        .find(|token| is_hysec_id(token))
        .map(str::to_owned)
}

fn is_hysec_id(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix("hysec_") else {
        return false;
    };
    suffix.len() == 20 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn assert_listed_session(output: &str, expected: &str) {
    assert!(
        output
            .lines()
            .any(|line| line.split_whitespace().next() == Some(expected)),
        "expected sessions output to list {expected}, got:\n{output}"
    );
}

fn assert_session_not_listed(output: &str, unexpected: &str) {
    assert!(
        !output
            .lines()
            .any(|line| line.split_whitespace().next() == Some(unexpected)),
        "expected sessions output not to list {unexpected}, got:\n{output}"
    );
}

fn exec_json_session(
    env: &IsolatedEnv,
    db: &PathBuf,
    prompt: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let exec = hya_command(env)
        .args(["--db"])
        .arg(db)
        .args(["exec", "--json", prompt])
        .output()?;
    assert_success("exec --json --db", &exec);
    let session = session_created_id(&String::from_utf8(exec.stdout)?)?
        .ok_or("missing session_created event")?;
    assert!(is_hysec_id(&session), "invalid session id: {session}");
    Ok(session)
}

fn session_created_id(output: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line)?;
        if value.pointer("/event/type") == Some(&Value::String("session_created".to_string())) {
            return Ok(value
                .pointer("/event/session")
                .and_then(Value::as_str)
                .map(str::to_owned));
        }
    }
    Ok(None)
}

/// The root session's `agent` id from its `session_created` event, if the
/// JSONL event stream (`--json`) contains one.
fn session_created_agent(output: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line)?;
        if value.pointer("/event/type") == Some(&Value::String("session_created".to_string())) {
            return Ok(value
                .pointer("/event/agent")
                .and_then(Value::as_str)
                .map(str::to_owned));
        }
    }
    Ok(None)
}

/// Write a minimal offline-provider `config.yaml` (matches the first-run
/// starter shape) with `default_agent` set to `agent_id`, so headless
/// commands resolve their root session's agent from config without needing a
/// live provider.
fn write_config_with_default_agent(
    env: &IsolatedEnv,
    agent_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let config_dir = env.xdg_config.join("hya");
    std::fs::create_dir_all(&config_dir)?;
    std::fs::write(
        config_dir.join("config.yaml"),
        format!(
            "default_model: hya/offline\n\
             providers: {{}}\n\
             mcp: {{}}\n\
             plugins: {{}}\n\
             permission:\n  model: default\n  rules: []\n\
             default_agent: {agent_id}\n"
        ),
    )?;
    Ok(())
}

fn assert_tail_replays_session(
    output: &str,
    expected: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut saw_session_created = false;
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line)?;
        if let Some(session) = value.pointer("/event/session").and_then(Value::as_str) {
            assert_eq!(session, expected, "tail replayed event for another session");
        }
        if value.pointer("/event/type") == Some(&Value::String("session_created".to_string())) {
            saw_session_created = true;
        }
    }
    assert!(
        saw_session_created,
        "tail-session did not replay session_created event:\n{output}"
    );
    Ok(())
}
