use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn agent_list_prints_opencode_native_agent_shape() -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_hya-backend"))
        .args(["agent", "list"])
        .output()?;

    assert!(
        output.status.success(),
        "agent list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
fn exec_persists_session_when_database_is_supplied() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = temp_dir("hya-backend-exec-db")?;
    let db = tmp.join("hya.db");

    let exec = Command::new(env!("CARGO_BIN_EXE_hya-backend"))
        .args(["--pure", "--db"])
        .arg(&db)
        .args(["exec", "Persist this CLI session"])
        .output()?;

    assert!(
        exec.status.success(),
        "exec failed: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    let sessions = Command::new(env!("CARGO_BIN_EXE_hya-backend"))
        .args(["sessions", "--pure", "--db"])
        .arg(&db)
        .output()?;

    assert!(
        sessions.status.success(),
        "sessions failed: {}",
        String::from_utf8_lossy(&sessions.stderr)
    );
    let stdout = String::from_utf8(sessions.stdout)?;
    assert!(
        stdout.contains("hysec_"),
        "expected persisted hysec session, got: {stdout}"
    );
    Ok(())
}

fn temp_dir(prefix: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&dir)?;
    Ok(dir)
}
