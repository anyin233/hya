//! CLI integration tests for `hya loop` routed through `hya/loop`'s `LoopDriver`,
//! including `--bare` and disabled `hya/loop` error reporting.

use crate::common::{Scratch, TestResult};

#[test]
fn loop_succeeds_with_until_predicate_and_errors_cleanly_when_bare_or_disabled() -> TestResult {
    let scratch = Scratch::new("loop-cli")?;

    // 1. Full mode: `hya loop --until true` succeeds via `hya/loop` `LoopDriver`.
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

    // 2. `--bare` mode: `hya --bare loop` returns a clear actionable error.
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
        bare_stderr.contains("hya/loop") && bare_stderr.contains("--bare"),
        "bare error must be actionable: {bare_stderr}"
    );

    // 3. `config.yaml` with `hya/loop` disabled returns a clear actionable error.
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
        disabled_stderr.contains("hya/loop"),
        "disabled error must mention hya/loop: {disabled_stderr}"
    );

    Ok(())
}
