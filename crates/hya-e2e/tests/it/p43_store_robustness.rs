//! Whole-process hostile-storage E2E tests.
//!
//! Exercises the real `hya` binary under external storage faults that an
//! outside test process can apply cleanly (external SQLite locks, read-only /
//! unopenable paths, database replacement mid-run, and server/short-command
//! exit durability reporting). Pure in-process disk stalls (`xWrite`) and
//! `ENOSPC` faults are covered by `crates/hya-store/tests/fault_injection.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child as StdChild, Command as StdCommand, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hya_e2e::{FakeLlm, ScriptStep, default_backend_bin, parse_listen_url, text_step, tool_step};
use hya_proto::{
    AgentName, Envelope, Event, ModelRef, PartProjection, Projection, Role, SessionId, SessionKind,
};
use hya_sdk::Sdk;
use hya_store::SessionStore;
use serde_json::json;
use sqlx::Connection as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
use tokio::process::Command;

static NEXT_CASE: AtomicU64 = AtomicU64::new(1);

fn hya_binary() -> PathBuf {
    let candidate = default_backend_bin();
    if candidate.is_file() {
        return candidate;
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(deps_dir) = exe.parent()
        && let Some(profile_dir) = deps_dir.parent()
    {
        let sibling = profile_dir.join("hya");
        if sibling.is_file() {
            return sibling;
        }
    }
    panic!(
        "hya binary missing at {} (run: cargo build -p hya-backend --bin hya)",
        candidate.display()
    );
}

struct IsolatedRunEnv {
    root: PathBuf,
    project: PathBuf,
    home: PathBuf,
    xdg_config_home: PathBuf,
    xdg_data_home: PathBuf,
    xdg_state_home: PathBuf,
    xdg_cache_home: PathBuf,
    binary: PathBuf,
    protected_dirs: Vec<PathBuf>,
}

impl IsolatedRunEnv {
    fn new(label: &str, fake_base_url: Option<&str>) -> Self {
        let id = NEXT_CASE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "hya-e2e-robust-{label}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let project = root.join("project");
        let home = root.join("home");
        let xdg_config_home = root.join("config");
        let xdg_data_home = root.join("data");
        let xdg_state_home = root.join("state");
        let xdg_cache_home = root.join("cache");
        let hya_cfg = xdg_config_home.join("hya");
        let auth_dir = hya_cfg.join("auth");
        for dir in [
            &project,
            &home,
            &auth_dir,
            &xdg_data_home,
            &xdg_state_home,
            &xdg_cache_home,
        ] {
            fs::create_dir_all(dir).expect("create isolated test dir");
        }
        fs::write(project.join("README.md"), "hya-e2e-robust\n").expect("seed readme");

        if let Some(base_url) = fake_base_url {
            let config = format!(
                r#"schema_version: 2
default_model: fake/model
providers:
  fake:
    kind: openai-compatible
    base_url: {base_url}
    api_key: e2e-test-key
    models:
    - id: model
plugins: {{}}
permission:
  model: allow
  rules: []
"#
            );
            fs::write(hya_cfg.join("config.yaml"), config).expect("write config.yaml");
            fs::write(auth_dir.join("fake.yaml"), "token: e2e-test-key\n").expect("write auth");
        }

        Self {
            root,
            project,
            home,
            xdg_config_home,
            xdg_data_home,
            xdg_state_home,
            xdg_cache_home,
            binary: hya_binary(),
            protected_dirs: Vec::new(),
        }
    }

    fn make_read_only_dir(&mut self, name: &str) -> PathBuf {
        let dir = self.root.join(name);
        fs::create_dir_all(&dir).expect("create dir before chmod");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).expect("chmod 0500");
        self.protected_dirs.push(dir.clone());
        dir
    }

    fn command(&self, db: &Path) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("--db")
            .arg(db)
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg_config_home)
            .env("XDG_DATA_HOME", &self.xdg_data_home)
            .env("XDG_STATE_HOME", &self.xdg_state_home)
            .env("XDG_CACHE_HOME", &self.xdg_cache_home)
            .env_remove("HYA_MODEL")
            .kill_on_drop(true);
        cmd
    }

    fn exec_command(&self, db: &Path, bare: bool, json: bool, prompt: &str) -> Command {
        let mut cmd = self.command(db);
        if !bare {
            cmd.arg("--yolo");
        }
        cmd.args(["--model", "fake/model", "exec"]);
        if bare {
            cmd.arg("--bare");
        }
        if json {
            cmd.arg("--json");
        }
        cmd.arg(prompt);
        cmd
    }
}

impl Drop for IsolatedRunEnv {
    fn drop(&mut self) {
        for dir in self.protected_dirs.iter().rev() {
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn assistant_text(projection: &Projection) -> String {
    projection
        .session
        .messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| m.parts.iter())
        .filter_map(|p| match p {
            PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn assert_pure_jsonl_with_answer(stdout: &str, expected: &str) -> Vec<Envelope> {
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!lines.is_empty(), "expected non-empty JSONL stdout");
    let mut envelopes = Vec::with_capacity(lines.len());
    for (idx, line) in lines.iter().enumerate() {
        let env: Envelope = serde_json::from_str(line).unwrap_or_else(|err| {
            panic!("stdout line {idx} is not a valid Envelope JSONL line ({err}): {line}")
        });
        envelopes.push(env);
    }
    let folded = Projection::from_events(&envelopes);
    let reply = assistant_text(&folded);
    assert!(
        reply.contains(expected),
        "folded JSONL reply missing {expected:?}: {reply:?}"
    );
    envelopes
}

fn assert_jsonl_matches_disk(stdout_envs: &[Envelope], disk_envs: &[Envelope], mode: &str) {
    let mut seen = HashSet::with_capacity(stdout_envs.len());
    for env in stdout_envs {
        assert!(
            seen.insert(env.seq.0),
            "{mode}: duplicate seq {} in stdout JSONL",
            env.seq.0
        );
    }
    let mut sorted = stdout_envs.to_vec();
    sorted.sort_by_key(|env| env.seq.0);
    assert_eq!(
        sorted.len(),
        disk_envs.len(),
        "{mode}: stdout JSONL envelope count {} != disk replay count {}",
        sorted.len(),
        disk_envs.len()
    );
    for (stdout_env, disk_env) in sorted.iter().zip(disk_envs.iter()) {
        assert_eq!(
            stdout_env.seq, disk_env.seq,
            "{mode}: stdout seq {:?} != disk seq {:?}",
            stdout_env.seq, disk_env.seq
        );
        assert_eq!(
            serde_json::to_value(stdout_env).expect("serialize stdout envelope"),
            serde_json::to_value(disk_env).expect("serialize disk envelope"),
            "{mode}: stdout envelope seq {} differs from disk envelope",
            disk_env.seq.0
        );
    }
}

fn assert_plain_output(output: &Output, expected: &str, mode: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{mode} exited with {:?}; stderr={stderr}",
        output.status.code()
    );
    assert!(
        stdout.contains(expected),
        "{mode} stdout missing {expected:?}: stdout={stdout:?} stderr={stderr:?}"
    );
}

async fn assert_disk_replay_complete(db: &Path, expected: &str, mode: &str) -> Vec<Envelope> {
    let ro = SessionStore::connect_read_only(db.to_str().unwrap())
        .await
        .unwrap_or_else(|err| panic!("{mode}: read-only open of {} failed: {err}", db.display()));
    let sessions = ro.list_sessions().await.expect("list sessions");
    assert_eq!(
        sessions.len(),
        1,
        "{mode}: expected 1 session on disk, got {}",
        sessions.len()
    );
    let envelopes = ro.replay(sessions[0].session).await.expect("replay");
    assert!(!envelopes.is_empty(), "{mode}: empty disk event log");
    let mut prev = 0;
    for env in &envelopes {
        assert!(
            env.seq.0 > prev,
            "{mode}: non-monotonic or duplicate seq {} after {prev}",
            env.seq.0
        );
        prev = env.seq.0;
    }
    let folded = Projection::from_events(&envelopes);
    let reply = assistant_text(&folded);
    assert!(
        reply.contains(expected),
        "{mode}: disk replay assistant text missing {expected:?}: {reply:?}"
    );
    envelopes
}

fn sync_bash_script(ready: &Path, go: &Path, final_answer: &str) -> Vec<ScriptStep> {
    let cmd = format!(
        "touch '{}' && i=0 && while [ ! -f '{}' ]; do i=$((i+1)); [ $i -lt 400 ] || exit 1; sleep 0.05; done && echo synced",
        ready.display(),
        go.display()
    );
    vec![
        tool_step("bash", json!({ "command": cmd })),
        // Bare mode triggers a single `[hya] Completion check` round after a tool call
        // and prints the last step's text; full mode stops after the first plain answer.
        text_step(format!("Done. {final_answer}")),
        text_step(format!("Done. {final_answer}")),
    ]
}

async fn wait_for_sync_file(ready: &Path, child: &mut tokio::process::Child, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready.exists() {
        if let Some(status) = child.try_wait().expect("try_wait child") {
            panic!("{label}: hya exited early with {status} before creating ready marker");
        }
        assert!(
            Instant::now() < deadline,
            "{label}: timed out waiting for ready marker {}",
            ready.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Fast (< 1.5 s) PR-gate smoke for T2.54:
/// - healthy `hya exec --json` persists cleanly with empty stderr and pure JSONL matching disk replay,
/// - healthy short writing command (`hya sessions archive`) drains cleanly with empty stderr,
/// - strict `hya serve` fails fast on an unopenable DB without falling back to memory,
/// - headless `hya exec` against an unopenable DB falls back in memory and reports exit durability.
#[tokio::test]
async fn p43_clean_exit_durability_and_strict_open_smoke() {
    // 1. Healthy `exec --json` persists cleanly with empty stderr and exact JSONL/disk parity.
    {
        let fake = FakeLlm::start(vec![text_step("CLEAN_SMOKE_OK")])
            .await
            .expect("start fake llm");
        let env = IsolatedRunEnv::new("smoke-clean", Some(&fake.base_url()));
        let db = env.root.join("hya.db");
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            env.exec_command(&db, false, true, "healthy durability smoke")
                .output(),
        )
        .await
        .expect("clean exec timed out")
        .expect("spawn clean exec");
        drop(fake);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "stderr={stderr}");
        assert!(
            stderr.trim().is_empty(),
            "healthy exec must emit no durability warning: {stderr}"
        );
        let stdout_envs = assert_pure_jsonl_with_answer(&stdout, "CLEAN_SMOKE_OK");
        let disk_envs = assert_disk_replay_complete(&db, "CLEAN_SMOKE_OK", "smoke-clean").await;
        assert_jsonl_matches_disk(&stdout_envs, &disk_envs, "smoke-clean");

        // Healthy short writing command (`sessions archive`) drains cleanly with empty stderr.
        let session = stdout_envs[0].event.session().expect("session id");
        let archive = tokio::time::timeout(
            Duration::from_secs(15),
            env.command(&db)
                .args(["sessions", "archive", &session.to_string()])
                .output(),
        )
        .await
        .expect("archive timed out")
        .expect("spawn archive");
        let archive_err = String::from_utf8_lossy(&archive.stderr);
        assert!(archive.status.success(), "stderr={archive_err}");
        assert!(
            archive_err.trim().is_empty(),
            "healthy archive must emit no warning: {archive_err}"
        );
        let ro = SessionStore::connect_read_only(db.to_str().unwrap())
            .await
            .expect("read-only reopen");
        assert!(
            ro.read_projection(session)
                .await
                .unwrap()
                .session
                .archived_at_millis()
                .is_some()
        );
    }

    // 2. Strict `hya serve` rejects an unopenable DB fast without memory fallback, while
    // headless `hya exec` falls back in memory and prints durability notices on stderr.
    {
        let fake = FakeLlm::start(vec![text_step("FALLBACK_SMOKE_OK")])
            .await
            .expect("start fake llm");
        let env = IsolatedRunEnv::new("smoke-unopenable", Some(&fake.base_url()));
        let dir_db = env.root.join("db-as-directory");
        fs::create_dir(&dir_db).expect("create dir blocker");

        let fg = tokio::time::timeout(
            Duration::from_secs(15),
            env.command(&dir_db)
                .args(["serve", "--bind", "127.0.0.1:0"])
                .output(),
        )
        .await
        .expect("strict serve timed out")
        .expect("spawn strict serve");
        let fg_stderr = String::from_utf8_lossy(&fg.stderr);
        assert!(!fg.status.success());
        assert!(
            !fg_stderr.trim().is_empty() && !fg_stderr.contains("running in memory"),
            "strict serve stderr={fg_stderr}"
        );

        let fallback = tokio::time::timeout(
            Duration::from_secs(15),
            env.exec_command(&dir_db, false, true, "fallback smoke")
                .output(),
        )
        .await
        .expect("fallback exec timed out")
        .expect("spawn fallback exec");
        drop(fake);
        let fb_stdout = String::from_utf8_lossy(&fallback.stdout);
        let fb_stderr = String::from_utf8_lossy(&fallback.stderr);
        assert!(fallback.status.success(), "stderr={fb_stderr}");
        assert_pure_jsonl_with_answer(&fb_stdout, "FALLBACK_SMOKE_OK");
        assert!(
            fb_stderr.contains("running in memory") && fb_stderr.contains("nothing was persisted"),
            "fallback stderr missing durability notices: {fb_stderr}"
        );
    }
}

#[tokio::test]
#[ignore = "nightly OS fault/soak"]
async fn p43_external_write_lock_bare_and_full_recover_and_persist() {
    for (bare, json) in [(true, false), (true, true), (false, false), (false, true)] {
        let mode = format!("bare={bare},json={json}");
        let sync_dir = IsolatedRunEnv::new("lock-sync", None);
        let ready1 = sync_dir.root.join("step1.ready");
        let go1 = sync_dir.root.join("step1.go");
        let ready2 = sync_dir.root.join("step2.ready");

        let cmd1 = format!(
            "touch '{}' && i=0 && while [ ! -f '{}' ]; do i=$((i+1)); [ $i -lt 400 ] || exit 1; sleep 0.05; done && echo step1-synced",
            ready1.display(),
            go1.display()
        );
        let cmd2 = format!("touch '{}' && echo step2-ran", ready2.display());
        let script = vec![
            tool_step("bash", json!({ "command": cmd1 })),
            tool_step("bash", json!({ "command": cmd2 })),
            text_step("Done. LOCK_RECOVERED_OK"),
            text_step("Done. LOCK_RECOVERED_OK"),
        ];

        let fake = FakeLlm::start(script).await.expect("start fake llm");
        let env = IsolatedRunEnv::new("lock", Some(&fake.base_url()));
        let db = env.root.join("hya.db");

        let mut cmd = env.exec_command(&db, bare, json, "run step and finish");
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn hya");

        wait_for_sync_file(&ready1, &mut child, &mode).await;

        // Acquire an external WAL EXCLUSIVE write lock and release step1.go.
        // Wait for step2.ready while holding EXCLUSIVE: that causally proves
        // an append after the lock degraded past WRITE_TIMEOUT (2s) and the
        // run progressed into tool 2 without waiting for COMMIT.
        let opts = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(false)
            .busy_timeout(Duration::from_secs(5));
        let mut external = sqlx::SqliteConnection::connect_with(&opts)
            .await
            .unwrap_or_else(|e| panic!("{mode}: connect external sqlite: {e}"));
        sqlx::query("BEGIN EXCLUSIVE")
            .execute(&mut external)
            .await
            .expect("BEGIN EXCLUSIVE");
        fs::write(&go1, b"go").expect("write go marker");

        wait_for_sync_file(&ready2, &mut child, &mode).await;
        sqlx::query("COMMIT")
            .execute(&mut external)
            .await
            .expect("COMMIT");
        drop(external);

        let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
            .await
            .unwrap_or_else(|_| panic!("{mode}: hya timed out waiting for completion"))
            .expect("wait_with_output");
        drop(fake);

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout_envs = if json {
            assert!(output.status.success(), "{mode} failed: stderr={stderr}");
            Some(assert_pure_jsonl_with_answer(&stdout, "LOCK_RECOVERED_OK"))
        } else {
            assert_plain_output(&output, "LOCK_RECOVERED_OK", &mode);
            None
        };
        assert!(
            stderr.contains("recovered") && stderr.contains("all events persisted"),
            "{mode}: expected recovery durability line on stderr, got: {stderr}"
        );
        let disk_envs = assert_disk_replay_complete(&db, "LOCK_RECOVERED_OK", &mode).await;
        if let Some(stdout_envs) = stdout_envs {
            assert_jsonl_matches_disk(&stdout_envs, &disk_envs, &mode);
        }
    }
}

#[tokio::test]
#[ignore = "nightly OS fault/soak"]
async fn p43_unopenable_db_paths_run_in_memory_bare_and_full() {
    let is_non_root = unsafe { libc::geteuid() } != 0;
    for kind in ["regular-file-parent", "ro-0500-dir"] {
        if kind == "ro-0500-dir" && !is_non_root {
            continue;
        }
        for (bare, json) in [(true, false), (true, true), (false, false), (false, true)] {
            let mode = format!("{kind},bare={bare},json={json}");
            let fake = FakeLlm::start(vec![
                tool_step("bash", json!({ "command": "echo mem-step-ok" })),
                text_step("Done. IN_MEMORY_OK"),
                text_step("Done. IN_MEMORY_OK"),
            ])
            .await
            .expect("start fake llm");
            let mut env = IsolatedRunEnv::new("unopenable", Some(&fake.base_url()));
            let db = if kind == "ro-0500-dir" {
                env.make_read_only_dir("ro").join("hya.db")
            } else {
                let blocker = env.root.join("not-a-dir");
                fs::write(&blocker, b"file").expect("create file blocker");
                blocker.join("hya.db")
            };

            let output = tokio::time::timeout(
                Duration::from_secs(20),
                env.exec_command(&db, bare, json, "run in memory").output(),
            )
            .await
            .unwrap_or_else(|_| panic!("{mode}: hya timed out"))
            .expect("spawn hya");
            drop(fake);

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if json {
                assert!(output.status.success(), "{mode} failed: stderr={stderr}");
                assert_pure_jsonl_with_answer(&stdout, "IN_MEMORY_OK");
            } else {
                assert_plain_output(&output, "IN_MEMORY_OK", &mode);
            }
            assert!(
                stderr.contains("running in memory"),
                "{mode}: missing open fallback stderr notice: {stderr}"
            );
            assert!(
                stderr.contains("nothing was persisted"),
                "{mode}: missing exit fallback stderr notice: {stderr}"
            );
            assert!(
                !db.exists(),
                "{mode}: DB file should not exist at {}",
                db.display()
            );
        }
    }
}

#[tokio::test]
#[ignore = "nightly OS fault/soak"]
async fn p43_open_stall_exceeding_open_timeout_falls_back_to_memory() {
    for bare in [true, false] {
        let mode = format!("open-stall,bare={bare}");
        let fake = FakeLlm::start(vec![
            tool_step("bash", json!({ "command": "echo open-stall-ok" })),
            text_step("Done. OPEN_STALL_FALLBACK_OK"),
            text_step("Done. OPEN_STALL_FALLBACK_OK"),
        ])
        .await
        .expect("start fake llm");
        let env = IsolatedRunEnv::new("open-stall", Some(&fake.base_url()));
        let db = env.root.join("stalled.db");

        // Pre-create the file in DELETE (rollback) journal mode and hold an
        // EXCLUSIVE transaction so `PRAGMA journal_mode = WAL` inside
        // `connect_file` blocks past `OPEN_TIMEOUT` (5 s) and falls back.
        let opts = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Delete);
        let mut blocker = sqlx::SqliteConnection::connect_with(&opts)
            .await
            .expect("create rollback-journal db");
        sqlx::query("CREATE TABLE hold (x INTEGER)")
            .execute(&mut blocker)
            .await
            .expect("create table");
        sqlx::query("BEGIN EXCLUSIVE")
            .execute(&mut blocker)
            .await
            .expect("hold EXCLUSIVE open lock");

        let started = Instant::now();
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            env.exec_command(&db, bare, false, "run under stalled open")
                .output(),
        )
        .await
        .unwrap_or_else(|_| panic!("{mode}: hya exceeded 30s open fallback deadline"))
        .expect("spawn hya");
        let elapsed = started.elapsed();

        let _ = sqlx::query("ROLLBACK").execute(&mut blocker).await;
        drop(blocker);
        drop(fake);

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            elapsed < Duration::from_secs(25),
            "{mode}: expected fallback completion within 25s upper bound, got {elapsed:?}; stderr={stderr}"
        );
        assert_plain_output(&output, "OPEN_STALL_FALLBACK_OK", &mode);
        assert!(
            stderr.contains("running in memory") && stderr.contains("nothing was persisted"),
            "{mode}: missing fallback warnings in stderr: {stderr}"
        );
        assert!(
            stderr.contains("timeout"),
            "{mode}: expected timeout reason in stderr: {stderr}"
        );

        let ro_opts = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(false)
            .read_only(true);
        let mut ro_conn = sqlx::SqliteConnection::connect_with(&ro_opts)
            .await
            .expect("reopen pre-created db after fallback");
        let hold_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM hold")
            .fetch_one(&mut ro_conn)
            .await
            .expect("query hold table");
        assert_eq!(
            hold_rows, 0,
            "{mode}: pre-created hold table should remain untouched"
        );
        let event_log_tables: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'event_log'",
        )
        .fetch_one(&mut ro_conn)
        .await
        .expect("check event_log absence");
        assert_eq!(
            event_log_tables, 0,
            "{mode}: fallback open must not initialize event_log on stalled pre-created db"
        );
    }
}

#[tokio::test]
#[ignore = "nightly OS fault/soak"]
async fn p43_replaced_db_file_halts_writer_and_completes_bare_and_full() {
    for (bare, json) in [(true, false), (true, true), (false, false), (false, true)] {
        let mode = format!("replace,bare={bare},json={json}");
        let sync_dir = IsolatedRunEnv::new("replace-sync", None);
        let ready = sync_dir.root.join("step1.ready");
        let go = sync_dir.root.join("step1.go");

        let fake = FakeLlm::start(sync_bash_script(&ready, &go, "REPLACED_SURVIVED_OK"))
            .await
            .expect("start fake llm");
        let env = IsolatedRunEnv::new("replace", Some(&fake.base_url()));
        let db = env.root.join("hya.db");
        let orig = env.root.join("hya.db.orig");

        let mut cmd = env.exec_command(&db, bare, json, "run across replaced db");
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn hya");

        wait_for_sync_file(&ready, &mut child, &mode).await;

        // Swap out the active DB file for a sentinel file with a different inode.
        fs::rename(&db, &orig).expect("rename active db");
        fs::write(&db, b"SENTINEL_REPLACEMENT_FILE").expect("write sentinel file");
        fs::write(&go, b"go").expect("write go marker");

        let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
            .await
            .unwrap_or_else(|_| panic!("{mode}: hya timed out"))
            .expect("wait_with_output");
        drop(fake);

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if json {
            assert!(output.status.success(), "{mode} failed: stderr={stderr}");
            assert_pure_jsonl_with_answer(&stdout, "REPLACED_SURVIVED_OK");
        } else {
            assert_plain_output(&output, "REPLACED_SURVIVED_OK", &mode);
        }
        assert!(
            stderr.contains("unpersisted")
                && stderr.contains(&db.display().to_string())
                && stderr.contains("replaced"),
            "{mode}: expected replaced/unpersisted stderr report, got: {stderr}"
        );
        assert_eq!(
            fs::read(&db).expect("read sentinel file"),
            b"SENTINEL_REPLACEMENT_FILE",
            "{mode}: replacement file must remain untouched"
        );
    }
}

struct ChildGuard(Option<StdChild>);

impl ChildGuard {
    fn new(child: StdChild) -> Self {
        Self(Some(child))
    }

    fn id(&self) -> u32 {
        self.0.as_ref().expect("child present").id()
    }

    fn take_stdio(&mut self) -> (std::process::ChildStdout, std::process::ChildStderr) {
        let child = self.0.as_mut().expect("child present");
        (
            child.stdout.take().expect("child stdout"),
            child.stderr.take().expect("child stderr"),
        )
    }

    async fn wait_with_timeout(
        &mut self,
        timeout: Duration,
        label: &str,
    ) -> std::process::ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self
                .0
                .as_mut()
                .expect("child present")
                .try_wait()
                .expect("try_wait child");
            if let Some(status) = status {
                self.0 = None;
                return status;
            }
            if Instant::now() >= deadline {
                if let Some(mut child) = self.0.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                panic!("{label}: server child did not exit within {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

async fn assert_strict_open_rejects(env: &IsolatedRunEnv, bad_db: &Path, label: &str) {
    // Foreground `hya serve` must fail promptly with a reason and never fall back to memory.
    let fg = tokio::time::timeout(
        Duration::from_secs(15),
        env.command(bad_db)
            .args(["serve", "--bind", "127.0.0.1:0"])
            .output(),
    )
    .await
    .unwrap_or_else(|_| panic!("{label}: foreground serve timed out"))
    .expect("spawn foreground serve");
    let fg_stdout = String::from_utf8_lossy(&fg.stdout);
    let fg_stderr = String::from_utf8_lossy(&fg.stderr);
    assert!(
        !fg.status.success(),
        "{label}: foreground serve must fail on unopenable db: stdout={fg_stdout:?} stderr={fg_stderr:?}"
    );
    assert!(
        !fg_stderr.trim().is_empty(),
        "{label}: foreground serve must report open failure reason"
    );
    assert!(
        !fg_stderr.contains("running in memory") && !fg_stdout.contains("hya server listening on"),
        "{label}: foreground serve must not fall back to memory: stdout={fg_stdout:?} stderr={fg_stderr:?}"
    );

    // Detached `hya serve start --json` must fail promptly (never wait 60s on a memory daemon).
    let detached = tokio::time::timeout(
        Duration::from_secs(15),
        env.command(bad_db)
            .args(["serve", "start", "--json"])
            .output(),
    )
    .await
    .unwrap_or_else(|_| panic!("{label}: detached serve start timed out"))
    .expect("spawn detached serve start");
    let detached_stdout = String::from_utf8_lossy(&detached.stdout);
    let detached_stderr = String::from_utf8_lossy(&detached.stderr);
    assert!(
        !detached.status.success(),
        "{label}: detached serve start must fail on unopenable db: stdout={detached_stdout:?} stderr={detached_stderr:?}"
    );
    assert!(
        !detached_stdout.contains("running in memory")
            && !detached_stderr.contains("running in memory")
            && !detached_stderr.contains("did not answer within"),
        "{label}: detached serve start must fail promptly without memory fallback: stdout={detached_stdout:?} stderr={detached_stderr:?}"
    );
    let log_path = PathBuf::from(format!("{}.server.log", bad_db.display()));
    let log = fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        !log.contains("running in memory") && !log.contains("hya server listening on"),
        "{label}: detached server log must not show memory fallback: {log:?}"
    );

    // Short strict command (`hya sessions archive`) must also fail strictly without memory fallback.
    let random_session = SessionId::new().to_string();
    let archive = tokio::time::timeout(
        Duration::from_secs(15),
        env.command(bad_db)
            .args(["sessions", "archive", &random_session])
            .output(),
    )
    .await
    .unwrap_or_else(|_| panic!("{label}: sessions archive timed out"))
    .expect("spawn sessions archive");
    let archive_stderr = String::from_utf8_lossy(&archive.stderr);
    assert!(
        !archive.status.success(),
        "{label}: sessions archive must fail on unopenable db: stderr={archive_stderr:?}"
    );
    assert!(
        !archive_stderr.trim().is_empty() && !archive_stderr.contains("running in memory"),
        "{label}: sessions archive must fail with reason and no memory fallback: stderr={archive_stderr:?}"
    );
}

#[tokio::test]
#[ignore = "nightly OS fault/soak"]
async fn p43_serve_shutdown_and_short_command_report_exit_durability() {
    let is_non_root = unsafe { libc::geteuid() } != 0;

    // 1a. Strict open rejects unopenable DBs in foreground serve, detached serve start, and short strict commands.
    {
        let env = IsolatedRunEnv::new("serve-strict-dir", None);
        let dir_db = env.root.join("db-as-directory");
        fs::create_dir(&dir_db).expect("create directory db blocker");
        assert_strict_open_rejects(&env, &dir_db, "dir-db").await;
    }
    if is_non_root {
        let mut env = IsolatedRunEnv::new("serve-strict-ro", None);
        let ro_db = env.make_read_only_dir("ro").join("hya.db");
        assert_strict_open_rejects(&env, &ro_db, "ro-dir-db").await;
    }

    // 1b. File-backed foreground server across DB replacement queues active turn events and reports exit durability on SIGTERM.
    {
        let sync_dir = IsolatedRunEnv::new("serve-replace-sync", None);
        let ready = sync_dir.root.join("step1.ready");
        let go = sync_dir.root.join("step1.go");
        let fake = FakeLlm::start(sync_bash_script(&ready, &go, "SERVE_REPLACED_OK"))
            .await
            .expect("start fake llm");

        let env = IsolatedRunEnv::new("serve-replace", Some(&fake.base_url()));
        let healthy_db = env.root.join("hya.db");
        let orig_db = env.root.join("hya.db.orig");

        let spawned = StdCommand::new(&env.binary)
            .args(["--yolo", "--model", "fake/model", "--db"])
            .arg(&healthy_db)
            .args(["serve", "--bind", "127.0.0.1:0"])
            .current_dir(&env.project)
            .env("HOME", &env.home)
            .env("XDG_CONFIG_HOME", &env.xdg_config_home)
            .env("XDG_DATA_HOME", &env.xdg_data_home)
            .env("XDG_STATE_HOME", &env.xdg_state_home)
            .env("XDG_CACHE_HOME", &env.xdg_cache_home)
            .env_remove("HYA_MODEL")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn healthy hya serve");
        let mut guard = ChildGuard::new(spawned);
        let pid = guard.id() as libc::pid_t;
        let (stdout, stderr) = guard.take_stdio();

        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<String>();
        let stdout_handle = std::thread::spawn(move || {
            let mut tx = Some(ready_tx);
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(url) = parse_listen_url(&line)
                    && let Some(sender) = tx.take()
                {
                    let _ = sender.send(url.to_owned());
                }
            }
        });
        let stderr_handle = std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = std::io::Read::read_to_string(&mut BufReader::new(stderr), &mut buf);
            buf
        });

        let url = tokio::time::timeout(Duration::from_secs(20), ready_rx)
            .await
            .expect("serve became ready")
            .expect("ready url channel");

        let sdk = Sdk::for_local_daemon(&url, env.project.to_string_lossy().into_owned())
            .expect("build Sdk");
        let created = sdk
            .create_session(hya_api::v2::CreateSessionRequest {
                kind: hya_api::v2::SessionKind::Temporary as i32,
                title: "before-replace".into(),
                ..Default::default()
            })
            .await
            .expect("create session on healthy serve");
        assert!(healthy_db.is_file(), "healthy serve db must exist on disk");

        let prompt_sdk = Sdk::for_local_daemon(&url, env.project.to_string_lossy().into_owned())
            .expect("build prompt Sdk");
        let sid = created.id.clone();
        let prompt_task = tokio::spawn(async move {
            prompt_sdk
                .prompt(&sid, "run server turn across db replace")
                .await
        });

        let marker_deadline = Instant::now() + Duration::from_secs(20);
        while !ready.exists() {
            if let Some(status) = guard
                .0
                .as_mut()
                .expect("serve child")
                .try_wait()
                .expect("try_wait serve child")
            {
                panic!("serve-replace exited early with {status} before tool ready marker");
            }
            assert!(
                Instant::now() < marker_deadline,
                "serve-replace timed out waiting for tool ready marker"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Replace the server's active DB file mid-turn, then release the tool so the turn queues events in memory.
        fs::rename(&healthy_db, &orig_db).expect("rename active server db");
        fs::write(&healthy_db, b"SENTINEL_SERVE_REPLACE")
            .expect("write server replacement sentinel");
        fs::write(&go, b"go").expect("write go marker");

        let turn_info = tokio::time::timeout(Duration::from_secs(30), prompt_task)
            .await
            .expect("server turn finished within 30s")
            .expect("join prompt task")
            .expect("server turn across DB replacement must succeed");
        assert!(!turn_info.id.is_empty(), "expected non-empty turn id");

        let rc = unsafe { libc::kill(pid, libc::SIGTERM) };
        assert_eq!(rc, 0, "send SIGTERM to hya serve");

        let status = guard
            .wait_with_timeout(Duration::from_secs(20), "serve-replace SIGTERM")
            .await;
        drop(fake);
        let _ = stdout_handle.join();
        let stderr_text = stderr_handle.join().expect("join stderr reader");
        assert!(
            status.success(),
            "hya serve did not exit cleanly on SIGTERM: {status:?}; stderr={stderr_text}"
        );
        assert!(
            stderr_text.contains("unpersisted")
                && !stderr_text.contains("0 event")
                && stderr_text.contains("replaced")
                && stderr_text.contains(&healthy_db.display().to_string()),
            "hya serve stderr missing replaced/unpersisted durability report: {stderr_text}"
        );
        assert_eq!(
            fs::read(&healthy_db).expect("read server sentinel file"),
            b"SENTINEL_SERVE_REPLACE",
            "replacement file must remain untouched after serve exit"
        );
    }

    // 2b. Healthy short writing command (`hya sessions archive`) drains cleanly with no spurious stderr warning.
    {
        let env = IsolatedRunEnv::new("short-clean", None);
        let db = env.root.join("hya.db");
        let store = SessionStore::connect(db.to_str().unwrap())
            .await
            .expect("seed db");
        let session = SessionId::new();
        store
            .append_event(
                session,
                &Event::SessionCreated {
                    session,
                    parent: None,
                    agent: AgentName::new("hya"),
                    model: ModelRef::new("fake/model"),
                    workdir: env.project.to_string_lossy().into_owned(),
                    project: None,
                    kind: SessionKind::Project,
                },
            )
            .await
            .expect("seed session");
        drop(store);

        let output = tokio::time::timeout(
            Duration::from_secs(15),
            env.command(&db)
                .args(["sessions", "archive", &session.to_string()])
                .output(),
        )
        .await
        .expect("sessions archive finished")
        .expect("spawn sessions archive");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "sessions archive failed: stderr={stderr}"
        );
        assert!(
            stderr.trim().is_empty(),
            "healthy sessions archive should emit no durability warning on stderr: {stderr}"
        );

        let ro = SessionStore::connect_read_only(db.to_str().unwrap())
            .await
            .expect("open read-only after archive");
        let proj = ro
            .read_projection(session)
            .await
            .expect("read archived session");
        assert!(
            proj.session.archived_at_millis().is_some(),
            "session should be archived on disk"
        );
    }
}

#[tokio::test]
#[ignore = "nightly OS fault/soak"]
async fn p43_signal_exit_paths_drain_store() {
    for (bare, sig, expected_code) in [
        (true, libc::SIGINT, 130),
        (true, libc::SIGTERM, 143),
        (false, libc::SIGINT, 130),
        (false, libc::SIGTERM, 143),
    ] {
        let mode = format!("signal bare={bare},sig={sig}");
        let sync_dir = IsolatedRunEnv::new("sig-sync", None);
        let ready = sync_dir.root.join("started.marker");
        let fake = FakeLlm::start(vec![
            tool_step(
                "bash",
                json!({ "command": format!("touch '{}' && sleep 30", ready.display()) }),
            ),
            text_step("unreachable"),
            text_step("unreachable"),
        ])
        .await
        .expect("start fake llm");

        let env = IsolatedRunEnv::new("signal", Some(&fake.base_url()));
        let db = env.root.join("db-as-directory");
        fs::create_dir(&db).expect("create directory db blocker");

        let mut cmd = env.exec_command(&db, bare, true, "wait on tool then catch signal");
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn hya");

        wait_for_sync_file(&ready, &mut child, &mode).await;
        let pid = child.id().expect("child pid") as libc::pid_t;
        let rc = unsafe { libc::kill(pid, sig) };
        assert_eq!(rc, 0, "{mode}: send signal {sig}");

        let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
            .await
            .unwrap_or_else(|_| panic!("{mode}: timed out waiting after signal"))
            .expect("wait_with_output");
        drop(fake);

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("running in memory"),
            "{mode}: expected open fallback notice, got: {stderr}"
        );
        assert_eq!(
            output.status.code(),
            Some(expected_code),
            "{mode}: expected exit code {expected_code}, got {:?}; stderr={stderr}",
            output.status
        );
        assert!(
            stderr.contains("nothing was persisted"),
            "{mode}: expected exit durability report on stderr, got: {stderr}"
        );
    }

    // Full-mode startup window (MCP handshake before turn starts) also catches SIGTERM and drains store.
    let mcp_env = IsolatedRunEnv::new("sig-startup", None);
    let mcp_ready = mcp_env.root.join("mcp.started");
    let cfg = format!(
        "schema_version: 2\ndefault_model: hya/offline\nmcp:\n  slow:\n    command: [\"sh\", \"-c\", \"touch '{}' && sleep 30\"]\n",
        mcp_ready.display()
    );
    fs::write(mcp_env.xdg_config_home.join("hya/config.yaml"), cfg).expect("write mcp config");
    let db = mcp_env.root.join("db-as-directory");
    fs::create_dir(&db).expect("create directory db blocker");
    let mut cmd = mcp_env.command(&db);
    cmd.args(["--yolo", "--model", "hya/offline", "exec", "unreachable"]);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn full-mode startup window");
    wait_for_sync_file(&mcp_ready, &mut child, "full startup window").await;
    let pid = child.id().expect("child pid") as libc::pid_t;
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("startup window timed out on SIGTERM")
        .expect("wait_with_output");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(143),
        "full startup window SIGTERM must exit 143; stderr={stderr}"
    );
    assert!(
        stderr.contains("running in memory") && stderr.contains("nothing was persisted"),
        "full startup window SIGTERM missing fallback/exit report: {stderr}"
    );
}

#[tokio::test]
#[ignore = "nightly OS fault/soak"]
async fn p43_first_signal_during_exit_drain_reports_and_exits_with_signal_code() {
    for (bare, sig, expected_code) in [
        (true, libc::SIGINT, 130),
        (false, libc::SIGINT, 130),
        (true, libc::SIGTERM, 143),
        (false, libc::SIGTERM, 143),
    ] {
        let mode = format!("drain-signal bare={bare},sig={sig}");
        let sync_dir = IsolatedRunEnv::new("drain-sig-sync", None);
        let ready = sync_dir.root.join("step1.ready");
        let go = sync_dir.root.join("step1.go");

        let fake = FakeLlm::start(sync_bash_script(&ready, &go, "SIGNAL_DRAIN_ANSWER"))
            .await
            .expect("start fake llm");
        let env = IsolatedRunEnv::new("drain-sig", Some(&fake.base_url()));
        let db = env.root.join("hya.db");

        let mut cmd = StdCommand::new(&env.binary);
        cmd.arg("--db").arg(&db);
        if !bare {
            cmd.arg("--yolo");
        }
        cmd.args(["--model", "fake/model", "exec"]);
        if bare {
            cmd.arg("--bare");
        }
        cmd.arg("run tool then drain")
            .current_dir(&env.project)
            .env("HOME", &env.home)
            .env("XDG_CONFIG_HOME", &env.xdg_config_home)
            .env("XDG_DATA_HOME", &env.xdg_data_home)
            .env("XDG_STATE_HOME", &env.xdg_state_home)
            .env("XDG_CACHE_HOME", &env.xdg_cache_home)
            .env_remove("HYA_MODEL")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut guard = ChildGuard::new(cmd.spawn().expect("spawn hya"));
        let pid = guard.id() as libc::pid_t;
        let (stdout_pipe, stderr_pipe) = guard.take_stdio();

        let out_buf = Arc::new(Mutex::new(Vec::new()));
        let err_buf = Arc::new(Mutex::new(Vec::new()));
        let out_handle = {
            let out_buf = Arc::clone(&out_buf);
            std::thread::spawn(move || {
                let mut pipe = stdout_pipe;
                let mut chunk = [0_u8; 4096];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    out_buf.lock().unwrap().extend_from_slice(&chunk[..n]);
                }
            })
        };
        let err_handle = {
            let err_buf = Arc::clone(&err_buf);
            std::thread::spawn(move || {
                let mut pipe = stderr_pipe;
                let mut chunk = [0_u8; 4096];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    err_buf.lock().unwrap().extend_from_slice(&chunk[..n]);
                }
            })
        };

        let ready_deadline = Instant::now() + Duration::from_secs(20);
        while !ready.exists() {
            if let Some(status) = guard
                .0
                .as_mut()
                .expect("child present")
                .try_wait()
                .expect("try_wait")
            {
                panic!("{mode}: hya exited early with {status} before ready marker");
            }
            assert!(
                Instant::now() < ready_deadline,
                "{mode}: timed out waiting for ready marker"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let opts = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(false)
            .busy_timeout(Duration::from_secs(5));
        let mut external = sqlx::SqliteConnection::connect_with(&opts)
            .await
            .unwrap_or_else(|e| panic!("{mode}: connect external sqlite: {e}"));
        sqlx::query("BEGIN EXCLUSIVE")
            .execute(&mut external)
            .await
            .expect("BEGIN EXCLUSIVE");
        fs::write(&go, b"go").expect("write go marker");

        // Wait until the run finishes and prints its answer, then wait 1s so it enters the 30s store exit drain.
        let answer_deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if String::from_utf8_lossy(&out_buf.lock().unwrap()).contains("SIGNAL_DRAIN_ANSWER") {
                break;
            }
            if let Some(status) = guard
                .0
                .as_mut()
                .expect("child present")
                .try_wait()
                .expect("try_wait")
            {
                let stderr = String::from_utf8_lossy(&err_buf.lock().unwrap()).into_owned();
                panic!("{mode}: hya exited early ({status}) before answer; stderr={stderr}");
            }
            assert!(
                Instant::now() < answer_deadline,
                "{mode}: timed out waiting for final answer on stdout"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;

        let sent = Instant::now();
        let rc = unsafe { libc::kill(pid, sig) };
        assert_eq!(rc, 0, "{mode}: send signal {sig}");

        let status = guard
            .wait_with_timeout(Duration::from_secs(45), &mode)
            .await;
        let elapsed = sent.elapsed();
        let _ = sqlx::query("ROLLBACK").execute(&mut external).await;
        drop(external);
        drop(fake);
        let _ = out_handle.join();
        let _ = err_handle.join();

        let stderr = String::from_utf8_lossy(&err_buf.lock().unwrap()).into_owned();
        let report_lines = stderr
            .lines()
            .filter(|l| l.starts_with("hya: store "))
            .count();
        assert_eq!(
            status.code(),
            Some(expected_code),
            "{mode}: expected exit code {expected_code}, got {status:?} after {elapsed:?}; stderr={stderr}"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "{mode}: expected prompt signal exit during exit drain, took {elapsed:?}; stderr={stderr}"
        );
        assert!(
            !stderr.contains("interrupted again"),
            "{mode}: first signal during exit drain must not print second-interrupt banner; stderr={stderr}"
        );
        assert_eq!(
            report_lines, 1,
            "{mode}: expected 1 store durability report line on stderr, got {report_lines}; stderr={stderr}"
        );
        assert!(
            stderr.contains("unpersisted at exit"),
            "{mode}: expected unpersisted durability report on stderr, got: {stderr}"
        );
    }
}
