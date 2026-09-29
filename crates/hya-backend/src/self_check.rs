//! The self-proof of a `hya` build (docs/cli.md "Self-proof before a
//! restart", ADR-0028): `hya serve check` composes the complete runtime the
//! daemon would run, and `hya serve restart` requires that check of the
//! successor executable, plus every `--verify` command, before it touches the
//! running daemon.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use hya_app::runtime::{
    agent_base_with_model, build_session_engine, build_session_engine_pure, resolve_runtime_strict,
};
use hya_store::SessionStore;

/// How long a successor's `serve check` may take before the restart fails.
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);
/// Output lines kept from a failed check or verify command.
const TAIL_LINES: usize = 40;

/// Compose the runtime exactly as a daemon start does, against a private
/// `VACUUM INTO` snapshot of `db` (migrations and startup recovery run on the
/// copy), or an in-memory store when `db` does not exist yet. The live
/// database, its locks, and its discovery file are never touched.
pub(crate) async fn compose(db: &str, model: Option<String>, pure: bool) -> anyhow::Result<()> {
    let scratch = scratch_dir()?;
    let result = compose_in(&scratch, db, model, pure).await;
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

async fn compose_in(
    scratch: &Path,
    db: &str,
    model: Option<String>,
    pure: bool,
) -> anyhow::Result<()> {
    let store = if !db.is_empty() && Path::new(db).is_file() {
        let copy = scratch.join("snapshot.db");
        SessionStore::snapshot_database(Path::new(db), &copy)
            .await
            .with_context(|| format!("snapshot {db}"))?;
        let copy = copy
            .to_str()
            .context("the snapshot path is not UTF-8")?
            .to_owned();
        SessionStore::connect(&copy)
            .await
            .context("open the database snapshot (migrations)")?
    } else {
        SessionStore::connect_memory()
            .await
            .context("open an in-memory store")?
    };
    let runtime = resolve_runtime_strict(model)
        .await
        .context("load the configuration")?
        .with_pure(pure);
    let agent = agent_base_with_model(&runtime.model, None);
    let tools = (runtime.websearch, runtime.permission);
    let mut built = if pure {
        build_session_engine_pure(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            tools,
        )
        .await
    } else {
        build_session_engine(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            tools,
        )
        .await
    }
    .context("compose the runtime")?;
    built
        .shutdown()
        .await
        .context("shut the composed runtime down")?;
    Ok(())
}

fn scratch_dir() -> anyhow::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let dir = std::env::temp_dir().join(format!("hya-check-{}-{nanos:x}", std::process::id()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(&dir)
        .with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

/// `hya serve check`: print the result and exit non-zero on failure.
pub(crate) async fn cmd_check(db: &str, model: Option<String>, pure: bool, json: bool) -> ! {
    let exe = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    match compose(db, model, pure).await {
        Ok(()) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": true,
                        "version": env!("CARGO_PKG_VERSION"),
                        "exe": exe,
                    })
                );
            } else {
                println!(
                    "hya {} composes its runtime ({exe})",
                    env!("CARGO_PKG_VERSION")
                );
            }
            std::process::exit(0);
        }
        Err(error) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": false,
                        "version": env!("CARGO_PKG_VERSION"),
                        "exe": exe,
                        "error": format!("{error:#}"),
                    })
                );
            } else {
                eprintln!("hya serve check: {error:#}");
            }
            std::process::exit(1);
        }
    }
}

/// The restart gate: the successor `exe` must pass `serve check` against
/// `db`, then every `verify` command (`sh -c`, current directory) must exit
/// 0. Returns the `check` object reported by `restart --json`.
pub(crate) async fn restart_gate(
    exe: &Path,
    db: &str,
    model: Option<&str>,
    pure: bool,
    verify: &[String],
) -> anyhow::Result<serde_json::Value> {
    let mut check = tokio::process::Command::new(exe);
    check.args(["serve", "check", "--json", "--db", db]);
    if let Some(model) = model {
        check.args(["--model", model]);
    }
    if pure {
        check.arg("--pure");
    }
    let output = run_captured(check, Some(CHECK_TIMEOUT))
        .await
        .with_context(|| format!("run `{} serve check`", exe.display()))?;
    let report: Option<serde_json::Value> = serde_json::from_slice(output.stdout.trim_ascii()).ok();
    let passed = output.status_ok
        && report
            .as_ref()
            .is_some_and(|report| report["ok"] == serde_json::json!(true));
    if !passed {
        let reason = report
            .as_ref()
            .and_then(|report| report["error"].as_str().map(str::to_owned))
            .unwrap_or_else(|| output.describe());
        anyhow::bail!(
            "the new build {} failed its self-check; the running server was not touched:\n{reason}{}",
            exe.display(),
            tail_block(&output.stderr),
        );
    }
    for command in verify {
        let mut shell = tokio::process::Command::new("sh");
        shell.args(["-c", command]);
        let output = run_captured(shell, None)
            .await
            .with_context(|| format!("run the verify command `{command}`"))?;
        if !output.status_ok {
            anyhow::bail!(
                "the verify command `{command}` failed ({}); the running server was not touched:{}{}",
                output.describe(),
                tail_block(&output.stdout),
                tail_block(&output.stderr),
            );
        }
    }
    Ok(serde_json::json!({
        "ok": true,
        "exe": exe.to_string_lossy(),
        "version": report.and_then(|report| report["version"].as_str().map(str::to_owned)),
        "verified": verify,
    }))
}

struct Captured {
    status_ok: bool,
    code: Option<i32>,
    timed_out: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Captured {
    fn describe(&self) -> String {
        if self.timed_out {
            "timed out".to_owned()
        } else {
            self.code.map_or_else(
                || "killed by a signal".to_owned(),
                |code| format!("exit {code}"),
            )
        }
    }
}

async fn run_captured(
    mut command: tokio::process::Command,
    timeout: Option<Duration>,
) -> anyhow::Result<Captured> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command.spawn()?;
    let waited = child.wait_with_output();
    let output = match timeout {
        Some(limit) => match tokio::time::timeout(limit, waited).await {
            Ok(output) => output?,
            Err(_) => {
                return Ok(Captured {
                    status_ok: false,
                    code: None,
                    timed_out: true,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                });
            }
        },
        None => waited.await?,
    };
    Ok(Captured {
        status_ok: output.status.success(),
        code: output.status.code(),
        timed_out: false,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

/// The last [`TAIL_LINES`] lines of `bytes`, indented, or nothing.
fn tail_block(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let start = lines.len().saturating_sub(TAIL_LINES);
    let mut block = String::new();
    for line in &lines[start..] {
        block.push_str("\n  ");
        block.push_str(line);
    }
    block
}
