//! Integration tests for bare `hya` without a terminal and empty stdin exec prompt.

use std::time::{Duration, Instant};

use crate::common::{Scratch, TestResult};

#[test]
fn bare_hya_without_a_terminal_prints_the_banner_with_default_or_custom_port() -> TestResult {
    let scratch = Scratch::new("hya-bare-banner")?;
    let empty = scratch.0.join("empty");
    std::fs::create_dir_all(&empty)?;
    let started = Instant::now();
    // Unusable Bun and asset overrides: without a TTY they are never looked at.
    let output = scratch
        .hya()
        .env("BUN", empty.join("no-bun"))
        .env("HYA_TUI_DIR", &empty)
        .env("HYA_TUI_WEB_DIR", &empty)
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("a multi-agent coding agent"), "{stdout}");
    assert!(stdout.contains("http://127.0.0.1:3250"), "{stdout}");
    assert!(stderr.is_empty(), "{stderr}");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(!scratch.0.join("state/hya/hya.log").exists());

    let custom = scratch.hya().args(["--port", "4321"]).output()?;
    assert!(custom.status.success());
    assert!(String::from_utf8(custom.stdout)?.contains("http://127.0.0.1:4321"));
    Ok(())
}

#[test]
fn exec_rejects_an_empty_stdin_prompt() -> TestResult {
    let scratch = Scratch::new("hya-empty-stdin-prompt")?;
    let output = scratch.hya().args(["exec", "--bare", "-"]).output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("empty exec prompt on stdin"), "{stderr}");
    Ok(())
}
