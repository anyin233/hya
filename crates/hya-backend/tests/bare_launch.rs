//! Integration tests for bare `hya` without a terminal: it prints the
//! guidance banner and starts nothing (no server, no Bun, no WebUI). The
//! terminal path is covered by `packages/hya-tui-web/e2e/hya-bare.spec.ts`,
//! which runs `hya` on a real PTY.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn scratch(prefix: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn hya(root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hya"));
    command
        .env_clear()
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("NO_COLOR", "1")
        .current_dir(root)
        .stdin(Stdio::null());
    command
}

#[test]
fn bare_hya_without_a_terminal_prints_the_banner_and_starts_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let root = scratch("hya-bare-banner")?;
    let empty = root.join("empty");
    std::fs::create_dir_all(&empty)?;
    let started = Instant::now();
    // Unusable Bun and asset overrides: without a TTY they are never looked at.
    let output = hya(&root)
        .env("BUN", empty.join("no-bun"))
        .env("HYA_TUI_DIR", &empty)
        .env("HYA_TUI_WEB_DIR", &empty)
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        output.status.success(),
        "status {}\n{stdout}\n{stderr}",
        output.status
    );
    assert!(stdout.contains("a multi-agent coding agent"), "{stdout}");
    assert!(
        stdout.contains("Run `hya` in a terminal to start the TUI and the WebUI"),
        "{stdout}"
    );
    assert!(stdout.contains("http://127.0.0.1:3250"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(started.elapsed() < Duration::from_secs(10));
    // No log file: nothing was started.
    assert!(!root.join("state/hya/hya.log").exists());
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn bare_banner_names_the_requested_port() -> Result<(), Box<dyn std::error::Error>> {
    let root = scratch("hya-bare-port")?;
    let output = hya(&root).args(["--port", "4321"]).output()?;
    let stdout = String::from_utf8(output.stdout)?;
    assert!(output.status.success());
    assert!(stdout.contains("http://127.0.0.1:4321"), "{stdout}");
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn port_with_a_subcommand_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
    let root = scratch("hya-bare-port-subcommand")?;
    let output = hya(&root).args(["--port", "4321", "sessions"]).output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("--port only applies to bare `hya`"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
