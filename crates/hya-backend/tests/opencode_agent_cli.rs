use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

#[test]
fn agent_list_prints_opencode_native_agent_shape() -> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-backend-agent-list")?;
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
fn rendered_exec_db_persists_and_tail_replays_hysec_session()
-> Result<(), Box<dyn std::error::Error>> {
    let env = IsolatedEnv::new("hya-backend-rendered-exec-db")?;
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
    let env = IsolatedEnv::new("hya-backend-json-exec-db")?;
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
