//! CLI integration tests for `hya loop`, the command the `hya/loop` bundle
//! contributes: it runs the `hya/loop:loop` driver, and does not exist with
//! `--bare` or with `hya/loop` disabled.

use crate::common::{Scratch, TestResult};

#[test]
fn loop_succeeds_with_until_predicate_and_errors_cleanly_when_bare_or_disabled() -> TestResult {
    let scratch = Scratch::new("loop-cli")?;

    // 1. Full mode: `hya loop --until true` succeeds via the `hya/loop:loop` driver.
    let ok = scratch
        .hya()
        .args([
            "--model",
            "hya/offline",
            "loop",
            "--target",
            "verify loop works",
            "--until",
            "true",
            "--budget",
            "2",
        ])
        .output()?;
    let stdout = String::from_utf8_lossy(&ok.stdout);
    let stderr = String::from_utf8_lossy(&ok.stderr);
    assert!(
        ok.status.success(),
        "expected `hya loop --until true` to succeed:\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("loop outcome: Achieved { iterations: 1"),
        "unexpected stdout: {stdout}"
    );

    // 2. `--bare` mode: no built-in bundle contributes `loop`.
    let bare = scratch
        .hya()
        .args([
            "--bare",
            "--model",
            "hya/offline",
            "loop",
            "--target",
            "should fail in bare mode",
            "--until",
            "true",
        ])
        .output()?;
    let bare_stderr = String::from_utf8_lossy(&bare.stderr);
    assert!(
        !bare.status.success(),
        "expected `hya --bare loop` to fail cleanly"
    );
    assert!(
        bare_stderr.contains("unrecognized subcommand 'loop'"),
        "bare `hya loop` must be an unknown command: {bare_stderr}"
    );

    // 3. With `hya/loop` disabled in `config.yaml`, `loop` is unknown too.
    let cfg_dir = scratch.0.join("config/hya");
    std::fs::create_dir_all(&cfg_dir)?;
    std::fs::write(
        cfg_dir.join("config.yaml"),
        "schema_version: 2\nbundles:\n  builtin:\n    profile: full\n    disable: [hya/loop]\n",
    )?;
    let disabled = scratch
        .hya()
        .args([
            "--model",
            "hya/offline",
            "loop",
            "--target",
            "should fail when hya/loop disabled",
            "--until",
            "true",
        ])
        .output()?;
    let disabled_stderr = String::from_utf8_lossy(&disabled.stderr);
    assert!(
        !disabled.status.success(),
        "expected `hya loop` to fail when hya/loop is disabled"
    );
    assert!(
        disabled_stderr.contains("unrecognized subcommand 'loop'"),
        "disabled `hya loop` must be an unknown command: {disabled_stderr}"
    );

    Ok(())
}

/// With no bundle claiming `-p` (`--bare`), `hya -p` runs one headless turn
/// and prints its transcript; with `hya/goal` enabled it runs goal mode.
#[test]
fn prompt_runs_one_turn_when_bare_and_goal_mode_otherwise() -> TestResult {
    let scratch = Scratch::new("prompt-cli")?;
    let bare = scratch
        .hya()
        .args(["--bare", "--model", "hya/offline", "-p", "say hi"])
        .output()?;
    let stdout = String::from_utf8_lossy(&bare.stdout);
    assert!(
        bare.status.success(),
        "{}",
        String::from_utf8_lossy(&bare.stderr)
    );
    assert!(
        stdout.contains("say hi") && !stdout.contains("goal outcome"),
        "bare -p must print one turn's transcript: {stdout}"
    );

    let full = scratch
        .hya()
        .args([
            "--model",
            "hya/offline",
            "-p",
            "say hi",
            "--max-iterations",
            "1",
        ])
        .output()?;
    let stdout = String::from_utf8_lossy(&full.stdout);
    assert!(
        stdout.contains("goal outcome:"),
        "full -p must run goal mode: {stdout}\n{}",
        String::from_utf8_lossy(&full.stderr)
    );
    Ok(())
}
