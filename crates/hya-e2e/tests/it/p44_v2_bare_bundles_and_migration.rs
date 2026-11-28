//! Track P v2 E2E matrix scenarios (ADR-0039):
//! - `T2.56`: Pure `hya --bare` (`exec` and `serve`) from an isolated prefix with
//!   zero `bundles/builtin/` present on disk.
//! - `T2.57`: Selective `bundles.builtin.disable` matrix (`hya/hashline` falling back
//!   to standard `read`/`write`/`edit`/`grep`, `hya/network` air-gapped, and
//!   `hya/multi-agent` hiding `agent()` in Core Code Mode and multi-agent tools).
//! - `T2.58`: Standalone `hya-migration` CLI E2E (`check` -> `run` -> v2 `hya`
//!   replay -> SHA-256 verified `rollback`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use hya_api::v2 as pb;
use hya_e2e::{
    E2eEnvBuilder, default_backend_bin, default_migration_bin, text_step, tool_step,
    unique_temp_dir,
};
use hya_proto::{FinishReason, MessageId, OwnerRunId, PartId, Role, SessionId};
use serde_json::{Value, json};
use sqlx::Connection as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

struct TempCleanup(std::path::PathBuf);

impl Drop for TempCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn offered_tool_names(req: &Value) -> Vec<String> {
    req["tools"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

fn offered_tool_description(req: &Value, name: &str) -> Option<String> {
    req["tools"]
        .as_array()?
        .iter()
        .find(|t| t["function"]["name"].as_str() == Some(name))
        .and_then(|t| t["function"]["description"].as_str())
        .map(str::to_owned)
}

fn all_requests_json(reqs: &[Value], start: usize) -> String {
    reqs[start..]
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// T2.56 — Pure `hya --bare` (`exec` and `serve`) runs cleanly from an isolated
/// `<prefix>/bin/hya` directory with zero `bundles/builtin/` present on disk.
#[tokio::test]
async fn pure_hya_bare_exec_and_serve_without_builtin_bundles() {
    let scratch = TempCleanup(unique_temp_dir("hya-e2e-pure-bare").unwrap());
    let isolated_bin_dir = scratch.0.join("isolated-prefix/bin");
    let empty_bundles = scratch.0.join("empty-bundles");
    std::fs::create_dir_all(&isolated_bin_dir).unwrap();
    std::fs::create_dir_all(&empty_bundles).unwrap();
    let isolated_hya = isolated_bin_dir.join("hya");
    std::fs::copy(default_backend_bin(), &isolated_hya).unwrap();
    assert!(
        !scratch.0.join("isolated-prefix/bundles").exists(),
        "isolated bare prefix must have zero bundles/"
    );

    // 1. Pure `hya --bare exec` exercising standard `write`/`read`/`edit`/`grep`/`todo`.
    let headless = E2eEnvBuilder::new()
        .binary(isolated_hya.clone())
        .scripts(vec![
            tool_step(
                "write",
                json!({"path": "bare_notes.txt", "content": "alpha\nbeta\ngamma\n"}),
            ),
            tool_step("read", json!({"path": "bare_notes.txt"})),
            tool_step(
                "edit",
                json!({"path": "bare_notes.txt", "old": "beta", "new": "BARE_EDITED"}),
            ),
            tool_step(
                "grep",
                json!({"pattern": "BARE_EDITED", "path": "bare_notes.txt"}),
            ),
            tool_step(
                "todo",
                json!({"ops": [{"op": "add", "content": "Bare kernel task"}]}),
            ),
            text_step("PURE_BARE_EXEC_OK"),
        ])
        .build_headless()
        .await
        .expect("headless bare env");

    let bare_db = headless.root.join("pure-bare-exec.db");
    let mut exec_cmd = headless.command();
    exec_cmd
        .env("HYA_FIRST_PARTY_SOURCE_ROOT", &empty_bundles)
        .args(["--bare", "--model", "fake/model", "--yolo", "--db"])
        .arg(&bare_db)
        .args(["exec", "--json", "exercise pure bare primitives"]);
    let exec_out = tokio::time::timeout(Duration::from_secs(30), exec_cmd.output())
        .await
        .expect("pure bare exec timed out")
        .expect("spawn pure bare exec");
    assert!(
        exec_out.status.success(),
        "pure bare exec failed: stderr={}",
        String::from_utf8_lossy(&exec_out.stderr)
    );
    let exec_stdout = String::from_utf8_lossy(&exec_out.stdout);
    assert!(
        exec_stdout.contains("PURE_BARE_EXEC_OK"),
        "missing final text in bare exec JSONL: {exec_stdout}"
    );
    let edited_disk = std::fs::read_to_string(headless.project.join("bare_notes.txt")).unwrap();
    assert_eq!(edited_disk, "alpha\nBARE_EDITED\ngamma\n");

    let exec_reqs = headless.fake.requests().unwrap();
    assert!(!exec_reqs.is_empty());
    let exec_tools = offered_tool_names(&exec_reqs[0]);
    for core_tool in ["read", "write", "edit", "bash", "grep", "todo", "rewrite"] {
        assert!(
            exec_tools.iter().any(|t| t == core_tool),
            "pure bare exec missing core tool `{core_tool}`: {exec_tools:?}"
        );
    }
    for bundle_tool in [
        "webfetch",
        "websearch",
        "task",
        "list_agents",
        "send",
        "ls",
        "glob",
        "find",
        "workflow",
        "plan_exit",
    ] {
        assert!(
            !exec_tools.iter().any(|t| t == bundle_tool),
            "pure bare exec must not expose bundle tool `{bundle_tool}`: {exec_tools:?}"
        );
    }
    let exec_dump = all_requests_json(&exec_reqs, 1);
    assert!(
        exec_dump.contains("BARE_EDITED") && exec_dump.contains("Bare kernel task"),
        "expected bare grep and todo results in follow-up requests: {exec_dump}"
    );

    // 2. Pure `hya --bare serve` with zero `bundles/builtin/` present.
    let serve_env = E2eEnvBuilder::new()
        .binary(isolated_hya)
        .bare(true)
        .agent("hya-bare")
        .backend_env(
            "HYA_FIRST_PARTY_SOURCE_ROOT",
            empty_bundles.display().to_string(),
        )
        .project_file("serve_bare.txt", b"first\nSERVE_BARE_NEEDLE\n".to_vec())
        .scripts(vec![
            tool_step("read", json!({"path": "serve_bare.txt"})),
            tool_step("grep", json!({"pattern": "SERVE_BARE_NEEDLE"})),
            text_step("PURE_BARE_SERVE_OK"),
        ])
        .build()
        .await
        .expect("pure bare serve env");

    let session = serve_env.create_session().await.unwrap();
    let turn = serve_env
        .prompt(session, "inspect serve_bare.txt in bare server")
        .await
        .unwrap();
    assert!(
        turn.error_message.is_empty(),
        "bare serve turn error: {}",
        turn.error_message
    );
    assert_eq!(turn.state, pb::TurnState::Finished as i32);

    let serve_reqs = serve_env.fake.requests().unwrap();
    let serve_tools = offered_tool_names(&serve_reqs[0]);
    for core_tool in ["read", "write", "edit", "bash", "grep", "todo", "rewrite"] {
        assert!(
            serve_tools.iter().any(|t| t == core_tool),
            "pure bare serve missing core tool `{core_tool}`: {serve_tools:?}"
        );
    }
    for bundle_tool in ["webfetch", "task", "ls", "workflow", "send", "plan_exit"] {
        assert!(
            !serve_tools.iter().any(|t| t == bundle_tool),
            "pure bare serve must not expose bundle tool `{bundle_tool}`: {serve_tools:?}"
        );
    }
    let agents = serve_env.list_agents().await.unwrap();
    let agents_str = agents.to_string();
    assert!(
        agents_str.contains("hya-bare") && !agents_str.contains("hya-task"),
        "bare serve agents should only include hya-bare: {agents_str}"
    );
    let events = serve_env.events(session, None).await.unwrap();
    assert!(
        events.iter().any(|e| serde_json::to_string(e)
            .unwrap()
            .contains("PURE_BARE_SERVE_OK")),
        "bare serve events missing reply"
    );
}

/// T2.57 — Selective built-in bundle disable:
/// 1. `bundles.builtin.disable: [hya/hashline]` falls back cleanly to standard
///    `read`/`write`/`edit`/`grep` while keeping other bundles active.
/// 2. `bundles.builtin.disable: [hya/network]` removes `webfetch`/`websearch`
///    for air-gapped operation.
/// 3. `bundles.builtin.disable: [hya/multi-agent]` removes multi-agent tools and
///    hides/blocks `agent()` / `agents` inside Core Code Mode.
#[tokio::test]
async fn selective_builtin_bundle_disable_matrix() {
    // --- A: disable hya/hashline -> standard bare read/write/edit/grep ---
    {
        let env = E2eEnvBuilder::new()
            .disable_builtin_bundle("hya/hashline")
            .project_file("doc.txt", b"line-1\nreplace-me\nline-3\n".to_vec())
            .scripts(vec![
                tool_step("read", json!({"path": "doc.txt"})),
                tool_step(
                    "write",
                    json!({"path": "created.txt", "content": "STANDARD_WRITE_OK\n"}),
                ),
                // Standard bare `edit` schema takes `{path, old, new}` (not hashline `edits: [...]`).
                tool_step(
                    "edit",
                    json!({"path": "doc.txt", "old": "replace-me", "new": "STANDARD_EDIT_OK"}),
                ),
                tool_step(
                    "grep",
                    json!({"pattern": "STANDARD_EDIT_OK", "path": "doc.txt"}),
                ),
                text_step("HASHLINE_DISABLED_OK"),
            ])
            .build()
            .await
            .expect("disable hashline env");

        let session = env.create_session().await.unwrap();
        let turn = env
            .prompt(session, "use standard file tools without hashline")
            .await
            .unwrap();
        assert!(
            turn.error_message.is_empty(),
            "disable hashline error: {}",
            turn.error_message
        );
        assert_eq!(turn.state, pb::TurnState::Finished as i32);

        assert_eq!(
            std::fs::read_to_string(env.backend.project.join("doc.txt")).unwrap(),
            "line-1\nSTANDARD_EDIT_OK\nline-3\n"
        );
        assert_eq!(
            std::fs::read_to_string(env.backend.project.join("created.txt")).unwrap(),
            "STANDARD_WRITE_OK\n"
        );
        let reqs = env.fake.requests().unwrap();
        let tools = offered_tool_names(&reqs[0]);
        for expected in ["read", "write", "edit", "grep", "ls", "webfetch"] {
            assert!(
                tools.iter().any(|t| t == expected),
                "expected `{expected}` to remain offered with only hya/hashline disabled: {tools:?}"
            );
        }
        // Standard bare `edit` schema has required `old` & `new` properties (not `edits`).
        let edit_schema = reqs[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"] == "edit")
            .unwrap();
        let props = &edit_schema["function"]["parameters"]["properties"];
        assert!(
            props.get("old").is_some()
                && props.get("new").is_some()
                && props.get("edits").is_none(),
            "expected standard bare edit schema when hya/hashline is disabled: {edit_schema}"
        );
    }

    // --- B: disable hya/network -> air-gapped (no webfetch/websearch) ---
    {
        let env = E2eEnvBuilder::new()
            .disable_builtin_bundle("hya/network")
            .scripts(vec![
                tool_step("webfetch", json!({"url": "http://127.0.0.1:9/blocked"})),
                text_step("AIRGAPPED_OK"),
            ])
            .build()
            .await
            .expect("disable network env");

        let session = env.create_session().await.unwrap();
        let turn = env
            .prompt(session, "verify network tools disabled")
            .await
            .unwrap();
        assert!(
            turn.error_message.is_empty(),
            "air-gapped turn error: {}",
            turn.error_message
        );
        assert_eq!(turn.state, pb::TurnState::Finished as i32);

        let reqs = env.fake.requests().unwrap();
        let tools = offered_tool_names(&reqs[0]);
        assert!(
            !tools.iter().any(|t| t == "webfetch" || t == "websearch"),
            "air-gapped profile must omit webfetch/websearch: {tools:?}"
        );
        assert!(
            tools.iter().any(|t| t == "read") && tools.iter().any(|t| t == "task"),
            "other bundles must remain active: {tools:?}"
        );
    }

    // --- C: disable hya/multi-agent -> hides multi-agent tools & agent() in code-mode ---
    {
        let env = E2eEnvBuilder::new()
            .disable_builtin_bundle("hya/multi-agent")
            .code_mode("with_tools")
            .project_file("notes.txt", b"CODE_MODE_SINGLE_AGENT_OK\n".to_vec())
            .scripts(vec![
                tool_step(
                    "code-mode__run",
                    json!({
                        "script": "const r = await tools.read({ path: 'notes.txt' }); return { r, agentType: typeof agent, agentsType: typeof agents };"
                    }),
                ),
                tool_step(
                    "code-mode__run",
                    json!({
                        "script": "return await agent({ agent: 'hya-scout', prompt: 'blocked' });"
                    }),
                ),
                text_step("MULTI_AGENT_DISABLED_OK"),
            ])
            .build()
            .await
            .expect("disable multi-agent env");

        let session = env.create_session().await.unwrap();
        let turn = env
            .prompt(session, "verify code-mode without hya/multi-agent")
            .await
            .unwrap();
        assert!(
            turn.error_message.is_empty(),
            "disable multi-agent turn error: {}",
            turn.error_message
        );
        assert_eq!(turn.state, pb::TurnState::Finished as i32);

        let reqs = env.fake.requests().unwrap();
        let tools = offered_tool_names(&reqs[0]);
        for removed in ["task", "list_agents", "search_agent", "archive"] {
            assert!(
                !tools.iter().any(|t| t == removed),
                "multi-agent tool `{removed}` must be absent when hya/multi-agent is disabled: {tools:?}"
            );
        }
        assert!(
            tools.iter().any(|t| t == "code-mode__run"),
            "core code-mode__run must remain available when hya/multi-agent is disabled: {tools:?}"
        );
        let desc = offered_tool_description(&reqs[0], "code-mode__run")
            .expect("code-mode__run description");
        assert!(
            !desc.contains("agents.run"),
            "code-mode__run description must omit agents.run when hya/multi-agent is disabled: {desc}"
        );

        let followups = all_requests_json(&reqs, 1);
        assert!(
            followups.contains("CODE_MODE_SINGLE_AGENT_OK")
                && followups.contains("agentType")
                && followups.contains("undefined"),
            "expected tools.read to succeed and typeof agent === undefined: {followups}"
        );
        assert!(
            followups.contains("hya/multi-agent"),
            "expected blocked agent() script to mention hya/multi-agent: {followups}"
        );
    }
}

/// Seed a realistic pre-Phase-0 v1 SQLite database with a session, user+assistant
/// text containing `marker`, a legacy `context_status` event, and a `resident_actor_claim`.
async fn seed_v1_sessions_db(db_path: &Path, workdir: &Path, marker: &str) -> SessionId {
    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true);
    let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();

    let v1_schema = r#"
CREATE TABLE _sqlx_migrations (
    version BIGINT PRIMARY KEY,
    description TEXT NOT NULL,
    installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    success BOOLEAN NOT NULL,
    checksum BLOB NOT NULL,
    execution_time BIGINT NOT NULL
);
INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
VALUES (1, 'init', 1, X'01020304', 100);

CREATE TABLE session (
    id           BLOB PRIMARY KEY,
    parent_id    BLOB,
    agent        TEXT NOT NULL,
    model        TEXT NOT NULL,
    workdir      TEXT NOT NULL,
    title        TEXT,
    permission   TEXT NOT NULL DEFAULT '{}',
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    project_id   TEXT,
    kind         TEXT NOT NULL DEFAULT 'project' CHECK (kind IN ('project', 'temporary')),
    FOREIGN KEY (parent_id) REFERENCES session(id)
);
CREATE INDEX session_parent ON session(parent_id);
CREATE INDEX session_project ON session(project_id);

CREATE TABLE event_log (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   BLOB NOT NULL,
    payload      TEXT NOT NULL,
    ts           INTEGER NOT NULL
);
CREATE INDEX event_log_session ON event_log(session_id);

CREATE TABLE saved_permission (
    id           TEXT PRIMARY KEY,
    project_id   TEXT NOT NULL,
    action       TEXT NOT NULL,
    resource     TEXT NOT NULL,
    time_created INTEGER,
    UNIQUE(project_id, action, resource)
);

CREATE TABLE resident_actor_claim (
    actor_id        BLOB PRIMARY KEY,
    root_session_id BLOB NOT NULL,
    epoch           INTEGER NOT NULL CHECK (epoch > 0),
    owner_run_id    BLOB NOT NULL CHECK (length(owner_run_id) = 16),
    state           TEXT NOT NULL CHECK (state IN ('active', 'released'))
);

CREATE TABLE agent_model_preference (
    agent_id    TEXT PRIMARY KEY NOT NULL,
    provider_id TEXT NOT NULL,
    model_id    TEXT NOT NULL
);

CREATE TABLE open_assistant_message (
    session_id BLOB NOT NULL,
    message_id TEXT NOT NULL,
    PRIMARY KEY (session_id, message_id)
) WITHOUT ROWID;

CREATE TABLE projection_snapshot (
    session_id      BLOB PRIMARY KEY,
    reducer_version INTEGER NOT NULL,
    last_seq        INTEGER NOT NULL,
    payload         BLOB NOT NULL
);

CREATE TABLE file_blob (
    session_id BLOB NOT NULL,
    hash       TEXT NOT NULL,
    size       INTEGER NOT NULL,
    content    BLOB NOT NULL,
    PRIMARY KEY (session_id, hash)
) WITHOUT ROWID;

CREATE TABLE project (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    archived   INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE project_root (
    project_id TEXT NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    path       TEXT NOT NULL,
    ord        INTEGER NOT NULL,
    PRIMARY KEY (project_id, ord),
    UNIQUE (project_id, path)
);

CREATE TABLE model_effort_preference (
    provider_id TEXT NOT NULL,
    model_id    TEXT NOT NULL,
    effort      TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (provider_id, model_id)
);

CREATE TABLE pending_resume (
    session_id        BLOB PRIMARY KEY,
    message_id        TEXT NOT NULL,
    prompt_message_id BLOB,
    created_at        INTEGER NOT NULL
);

CREATE TABLE agent_effort_preference (
    agent_id   TEXT NOT NULL PRIMARY KEY,
    effort     TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
"#;
    sqlx::raw_sql(v1_schema).execute(&mut conn).await.unwrap();

    let session = SessionId::new();
    let child = SessionId::new();
    let owner = OwnerRunId::new();
    let user_msg = MessageId::new();
    let assistant_msg = MessageId::new();
    let user_part = PartId::new();
    let assistant_part = PartId::new();
    let workdir_str = workdir.display().to_string();

    sqlx::query(
        "INSERT INTO session (id, parent_id, agent, model, workdir, title, permission, created_at, updated_at, project_id, kind) \
         VALUES (?, NULL, 'hya-main', 'hya/offline', ?, 'Legacy v1 Session', '{}', 1000, 2000, NULL, 'project')",
    )
    .bind(session.storage_key())
    .bind(&workdir_str)
    .execute(&mut conn)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO resident_actor_claim (actor_id, root_session_id, epoch, owner_run_id, state) \
         VALUES (?, ?, 2, ?, 'active')",
    )
    .bind(child.storage_key())
    .bind(session.storage_key())
    .bind(owner.as_uuid().as_bytes().as_slice())
    .execute(&mut conn)
    .await
    .unwrap();

    let events = vec![
        json!({
            "type": "session_created",
            "session": session,
            "parent": null,
            "agent": "hya-main",
            "model": "hya/offline",
            "workdir": workdir_str,
            "project_id": null,
            "kind": "project"
        }),
        json!({
            "type": "message_started",
            "session": session,
            "message": user_msg,
            "role": Role::User,
            "agent": "hya-main",
            "model": "hya/offline"
        }),
        json!({
            "type": "text_replace",
            "session": session,
            "message": user_msg,
            "part": user_part,
            "text": "v1 user question"
        }),
        json!({
            "type": "message_finished",
            "session": session,
            "message": user_msg,
            "role": Role::User,
            "finish": FinishReason::Stop
        }),
        json!({
            "type": "message_started",
            "session": session,
            "message": assistant_msg,
            "role": Role::Assistant,
            "agent": "hya-main",
            "model": "hya/offline"
        }),
        json!({
            "type": "text_replace",
            "session": session,
            "message": assistant_msg,
            "part": assistant_part,
            "text": marker
        }),
        // Legacy v1 domain event that `hya-migration` converts to `Event::Extension`.
        json!({
            "type": "context_status",
            "session": session,
            "used_tokens": 1200,
            "budget_tokens": 8000,
            "context_window": 32000
        }),
        json!({
            "type": "message_finished",
            "session": session,
            "message": assistant_msg,
            "role": Role::Assistant,
            "finish": FinishReason::Stop
        }),
    ];

    for (idx, event) in events.into_iter().enumerate() {
        sqlx::query("INSERT INTO event_log (session_id, payload, ts) VALUES (?, ?, ?)")
            .bind(session.storage_key())
            .bind(serde_json::to_string(&event).unwrap())
            .bind(1000_i64 + idx as i64)
            .execute(&mut conn)
            .await
            .unwrap();
    }

    conn.close().await.unwrap();
    session
}

/// T2.58 — Standalone `hya-migration` CLI E2E:
/// `check` (read-only) -> `run` (with SHA-256 backup) -> v2 `hya` replay (`sessions` +
/// `tail-session`) -> `rollback` (byte-for-byte restoration + v2 rejection of restored v1 DB).
#[tokio::test]
async fn hya_migration_check_run_v2_replay_and_rollback() {
    let hya_bin = default_backend_bin();
    let migration_bin = default_migration_bin();
    assert!(
        hya_bin.is_file(),
        "hya binary missing at {}",
        hya_bin.display()
    );
    assert!(
        migration_bin.is_file(),
        "hya-migration binary missing at {} (run: cargo build -p hya-migration --bin hya-migration)",
        migration_bin.display()
    );

    let scratch = TempCleanup(unique_temp_dir("hya-e2e-migration").unwrap());
    let home = scratch.0.join("home");
    let config_dir = scratch.0.join("config/hya");
    let data_dir = scratch.0.join("data");
    let project_dir = scratch.0.join("project");
    let bundles_dir = project_dir.join(".hya/bundles");
    let legacy_bundle_dir = bundles_dir.join("acme-legacy");
    let backup_dir = scratch.0.join("migration-backup");

    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&legacy_bundle_dir).unwrap();

    let db_path = scratch.0.join("sessions.db");
    let config_path = config_dir.join("config.yaml");
    let tui_toml_path = config_dir.join("tui.toml");
    let bundle_yaml_path = legacy_bundle_dir.join("bundle.yaml");

    const MARKER: &str = "MIGRATED_V1_CONVERSATION_MARKER";
    let session_id = seed_v1_sessions_db(&db_path, &project_dir, MARKER).await;

    std::fs::write(
        &config_path,
        "default_model: hya/offline\nproviders: {}\ntools:\n  websearch:\n    enabled: false\n",
    )
    .unwrap();
    std::fs::write(
        &tui_toml_path,
        "theme = \"dark\"\n[layout]\npane_order = [\"subagents\", \"workflows\"]\n",
    )
    .unwrap();
    std::fs::write(
        &bundle_yaml_path,
        "kind: Bundle\nidentity:\n  id: acme/legacy\n  version: 1.0.0\n  publisher: acme\n",
    )
    .unwrap();

    // Snapshot original v1 file bytes before touching anything.
    let orig_db = std::fs::read(&db_path).unwrap();
    let orig_config = std::fs::read(&config_path).unwrap();
    let orig_tui = std::fs::read(&tui_toml_path).unwrap();
    let orig_bundle = std::fs::read(&bundle_yaml_path).unwrap();

    let run_hya = |args: &[&str]| {
        Command::new(&hya_bin)
            .args(args)
            .current_dir(&project_dir)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", scratch.0.join("config"))
            .env("XDG_DATA_HOME", &data_dir)
            .env("XDG_STATE_HOME", scratch.0.join("state"))
            .env("XDG_CACHE_HOME", scratch.0.join("cache"))
            .output()
            .expect("spawn hya")
    };

    let run_migration = |args: &[&str]| {
        Command::new(&migration_bin)
            .args(args)
            .current_dir(&project_dir)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", scratch.0.join("config"))
            .env("XDG_DATA_HOME", &data_dir)
            .output()
            .expect("spawn hya-migration")
    };

    // 1. Unmigrated v1 DB must be rejected cleanly by v2 `hya`.
    let db_str = db_path.display().to_string();
    let cfg_str = config_path.display().to_string();
    let tui_str = tui_toml_path.display().to_string();
    let bundles_str = bundles_dir.display().to_string();
    let backup_str = backup_dir.display().to_string();

    let pre_check_hya = run_hya(&["--db", &db_str, "sessions"]);
    assert!(
        !pre_check_hya.status.success(),
        "v2 hya must refuse unmigrated v1 database"
    );
    let pre_err = format!(
        "{}{}",
        String::from_utf8_lossy(&pre_check_hya.stdout),
        String::from_utf8_lossy(&pre_check_hya.stderr)
    );
    assert!(
        pre_err.contains("hya-migration"),
        "expected v1 DB rejection to direct user to hya-migration, got: {pre_err}"
    );

    // 2. `hya-migration check` detects v1 artifacts without mutating disk.
    let check_before = run_migration(&[
        "--json",
        "check",
        "--db",
        &db_str,
        "--config",
        &cfg_str,
        "--tui-toml",
        &tui_str,
        "--bundles-dir",
        &bundles_str,
    ]);
    assert!(
        check_before.status.success(),
        "hya-migration check failed: {}",
        String::from_utf8_lossy(&check_before.stderr)
    );
    let check_json: Value = serde_json::from_slice(&check_before.stdout).unwrap();
    assert_eq!(check_json["needs_migration"], true, "{check_json}");
    assert_eq!(std::fs::read(&db_path).unwrap(), orig_db);
    assert_eq!(std::fs::read(&config_path).unwrap(), orig_config);
    assert_eq!(std::fs::read(&tui_toml_path).unwrap(), orig_tui);
    assert_eq!(std::fs::read(&bundle_yaml_path).unwrap(), orig_bundle);

    // 3. `hya-migration run` writes SHA-256 backup manifest and atomically upgrades to v2.
    let run_out = run_migration(&[
        "--json",
        "run",
        "--db",
        &db_str,
        "--config",
        &cfg_str,
        "--tui-toml",
        &tui_str,
        "--bundles-dir",
        &bundles_str,
        "--backup-dir",
        &backup_str,
    ]);
    assert!(
        run_out.status.success(),
        "hya-migration run failed: {}",
        String::from_utf8_lossy(&run_out.stderr)
    );
    let run_json: Value = serde_json::from_slice(&run_out.stdout).unwrap();
    assert_eq!(run_json["db"]["migrated"], true, "{run_json}");
    assert_eq!(run_json["db"]["store_verified"], true, "{run_json}");
    assert_eq!(run_json["config"]["migrated"], true, "{run_json}");
    assert!(
        backup_dir.join("manifest.json").is_file(),
        "missing SHA-256 backup manifest"
    );

    // 4. Subsequent `hya-migration check` reports clean v2 state.
    let check_after = run_migration(&[
        "--json",
        "check",
        "--db",
        &db_str,
        "--config",
        &cfg_str,
        "--tui-toml",
        &tui_str,
        "--bundles-dir",
        &bundles_str,
    ]);
    assert!(check_after.status.success());
    let check_after_json: Value = serde_json::from_slice(&check_after.stdout).unwrap();
    assert_eq!(
        check_after_json["needs_migration"], false,
        "{check_after_json}"
    );

    // 5. Real v2 `hya` replays the migrated SQLite database cleanly.
    let sessions_out = run_hya(&["--db", &db_str, "sessions"]);
    assert!(
        sessions_out.status.success(),
        "v2 hya sessions failed on migrated DB: stderr={}",
        String::from_utf8_lossy(&sessions_out.stderr)
    );
    let sessions_stdout = String::from_utf8_lossy(&sessions_out.stdout);
    let session_str = session_id.to_string();
    assert!(
        sessions_stdout.contains(&session_str),
        "migrated session {session_str} missing from `hya sessions`: {sessions_stdout}"
    );

    let tail_out = run_hya(&["--db", &db_str, "tail-session", &session_str]);
    assert!(
        tail_out.status.success(),
        "v2 hya tail-session failed on migrated DB: stderr={}",
        String::from_utf8_lossy(&tail_out.stderr)
    );
    let tail_stdout = String::from_utf8_lossy(&tail_out.stdout);
    assert!(
        tail_stdout.contains(MARKER),
        "migrated assistant message marker missing from tail-session: {tail_stdout}"
    );
    assert!(
        tail_stdout.contains("\"type\":\"extension\"")
            && tail_stdout.contains("hya/compaction")
            && tail_stdout.contains("context_status"),
        "expected legacy context_status event to replay as v2 Event::Extension: {tail_stdout}"
    );

    // 6. `hya-migration rollback` verifies SHA-256 manifest and restores exact v1 bytes.
    let rollback_out = run_migration(&["--json", "rollback", "--backup", &backup_str]);
    assert!(
        rollback_out.status.success(),
        "hya-migration rollback failed: {}",
        String::from_utf8_lossy(&rollback_out.stderr)
    );
    let rollback_json: Value = serde_json::from_slice(&rollback_out.stdout).unwrap();
    assert!(
        rollback_json["verified_files"].as_u64().unwrap_or(0) >= 4,
        "expected >= 4 SHA-256 verified backup files: {rollback_json}"
    );

    assert_eq!(std::fs::read(&db_path).unwrap(), orig_db);
    assert_eq!(std::fs::read(&config_path).unwrap(), orig_config);
    assert_eq!(std::fs::read(&tui_toml_path).unwrap(), orig_tui);
    assert_eq!(std::fs::read(&bundle_yaml_path).unwrap(), orig_bundle);

    // Confirm restored DB is v1 again (rejected by v2 `hya`).
    let post_rollback_hya = run_hya(&["--db", &db_str, "sessions"]);
    assert!(
        !post_rollback_hya.status.success(),
        "rolled-back v1 DB must once again be rejected by v2 hya"
    );
}
