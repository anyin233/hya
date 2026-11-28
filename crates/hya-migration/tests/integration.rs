//! End-to-end integration tests for `hya-migration` (v1 -> v2 + SHA-256 rollback).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use hya_app::bundle_registry::{BundleInstallCandidate, BundleRegistry, NamespaceInstallPolicy};
use hya_bundle::{BundleSource, PreparedCatalog, SourceFile, prepare_package};
use hya_migration::backup::{file_sha256, sha256_hex, verify_backup_manifest};
use hya_migration::bundle_ids::user_bundle_config_path;
use hya_migration::bundle_v1_to_v2::{BundleCheckStatus, DiagnosticSeverity, RegistryCheckStatus};
use hya_migration::config_v1_to_v2::ConfigCheckStatus;
use hya_migration::db_v1_to_v2::DbCheckStatus;
use hya_migration::error::MigrationError;
use hya_migration::tui_v1_to_v2::{
    PANEL_SUBAGENT_VIEWER, PANEL_SUBAGENTS, PANEL_WORKFLOWS, TuiCheckStatus,
};
use hya_migration::{MigrationTargets, RunOptions, check_all, rollback, run_migration};
use hya_proto::{
    AgentName, MemberId, MessageId, OwnerRunId, PartId, PartProjection, Role, SessionId,
    ToolCallId, WorkflowRunId,
};
use hya_store::SessionStore;
use hya_tool::extension::{
    BUNDLE_MULTI_AGENT, context_status_projection, handoff_projection, members_projection,
    projection_reducers, team_projection, workflow_projection,
};
use serde_json::{Value as JsonValue, json};
use sqlx::Connection as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};

static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempWorkspace {
    root: PathBuf,
}

impl TempWorkspace {
    fn new(tag: &str) -> Self {
        let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "hya-migration-it-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct SeededV1Fixtures {
    targets: MigrationTargets,
    root_session: SessionId,
    child_session: SessionId,
    active_owner: OwnerRunId,
    tool_part: PartId,
    js_bundle_dir: PathBuf,
    md_bundle_dir: PathBuf,
    reserved_bundle_dir: PathBuf,
}

async fn seed_v1_database(db_path: &Path) -> (SessionId, SessionId, OwnerRunId, PartId) {
    let opts = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true);
    let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();

    // Exact v1 baseline schema (pre-Phase 0, with `resident_actor_claim`, no `schema_meta`,
    // no `bundle_claim` / `bundle_state`, and legacy `_sqlx_migrations` checksum).
    let v1_sql = r#"
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
    sqlx::raw_sql(v1_sql).execute(&mut conn).await.unwrap();

    let root = SessionId::new();
    let child = SessionId::new();
    let released_child = SessionId::new();
    let active_owner = OwnerRunId::new();
    let released_owner = OwnerRunId::new();
    let msg_id = MessageId::new();
    let tool_part = PartId::new();
    let tool_call = ToolCallId::new();
    let member_id = MemberId::new();
    let wf_run = WorkflowRunId::new();
    let project_id = hya_proto::ProjectId::new().to_string();

    // Seed project + project_root + permission + preferences + file_blob.
    sqlx::query("INSERT INTO project (id, name, created_at, updated_at, archived) VALUES (?, 'Demo', 1000, 1000, 0)")
        .bind(&project_id)
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO project_root (project_id, path, ord) VALUES (?, '/work/demo', 0)")
        .bind(&project_id)
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO saved_permission (id, project_id, action, resource, time_created) VALUES ('perm-1', ?, 'read', '/work/demo/**', 1000)")
        .bind(&project_id)
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO agent_model_preference (agent_id, provider_id, model_id) VALUES ('hya-main', 'openai', 'gpt-5')")
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO model_effort_preference (provider_id, model_id, effort, updated_at) VALUES ('openai', 'gpt-5', 'high', 1000)")
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO agent_effort_preference (agent_id, effort, updated_at) VALUES ('hya-main', 'medium', 1000)")
        .execute(&mut conn)
        .await
        .unwrap();

    let blob_bytes = b"hello v1 blob".to_vec();
    let blob_hash = sha256_hex(&blob_bytes);
    sqlx::query("INSERT INTO file_blob (session_id, hash, size, content) VALUES (?, ?, ?, ?)")
        .bind(root.storage_key())
        .bind(&blob_hash)
        .bind(blob_bytes.len() as i64)
        .bind(&blob_bytes)
        .execute(&mut conn)
        .await
        .unwrap();

    // Seed root + child sessions.
    sqlx::query("INSERT INTO session (id, parent_id, agent, model, workdir, title, permission, created_at, updated_at, project_id, kind) VALUES (?, NULL, 'hya-main', 'hya/offline', '/work/demo', 'Root Session', '{}', 1000, 2000, ?, 'project')")
        .bind(root.storage_key())
        .bind(&project_id)
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO session (id, parent_id, agent, model, workdir, title, permission, created_at, updated_at, project_id, kind) VALUES (?, ?, 'hya-task', 'hya/offline', '/work/demo', 'Child Session', '{}', 1100, 1900, ?, 'project')")
        .bind(child.storage_key())
        .bind(root.storage_key())
        .bind(&project_id)
        .execute(&mut conn)
        .await
        .unwrap();

    // Seed stale v1 projection_snapshot (must be dropped by migration).
    sqlx::query("INSERT INTO projection_snapshot (session_id, reducer_version, last_seq, payload) VALUES (?, 1, 5, X'deadbeef')")
        .bind(root.storage_key())
        .execute(&mut conn)
        .await
        .unwrap();

    // Seed v1 `resident_actor_claim` rows (1 active, 1 released).
    sqlx::query("INSERT INTO resident_actor_claim (actor_id, root_session_id, epoch, owner_run_id, state) VALUES (?, ?, 3, ?, 'active')")
        .bind(child.storage_key())
        .bind(root.storage_key())
        .bind(active_owner.as_uuid().as_bytes().as_slice())
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO resident_actor_claim (actor_id, root_session_id, epoch, owner_run_id, state) VALUES (?, ?, 1, ?, 'released')")
        .bind(released_child.storage_key())
        .bind(root.storage_key())
        .bind(released_owner.as_uuid().as_bytes().as_slice())
        .execute(&mut conn)
        .await
        .unwrap();

    // Seed v1 event_log: kernel events + ALL 4 v1 domain event families!
    let root_events: Vec<JsonValue> = vec![
        // Kernel events
        json!({
            "type": "session_created",
            "session": root,
            "parent": null,
            "agent": "hya-main",
            "model": "hya/offline",
            "workdir": "/work/demo",
            "project_id": project_id,
            "kind": "project"
        }),
        json!({
            "type": "message_started",
            "session": root,
            "message": msg_id,
            "role": Role::Assistant,
            "agent": "hya-main",
            "model": "hya/offline"
        }),
        json!({
            "type": "tool_input_start",
            "session": root,
            "message": msg_id,
            "part": tool_part,
            "call": tool_call,
            "name": "read"
        }),
        // hya/compaction v1 events
        json!({
            "type": "context_evicted",
            "session": root,
            "evicted_parts": 1,
            "parts": [{"part": tool_part, "notice": "[moved to artifact://abc]"}],
            "tokens_before": 9000,
            "tokens_after": 3000,
            "threshold": 8000
        }),
        json!({
            "type": "context_status",
            "session": root,
            "tokens": 3000,
            "source": "estimate",
            "mode": "auto",
            "threshold": 8000
        }),
        // hya/workflow v1 events
        json!({
            "type": "workflow_selected",
            "session": root,
            "workflow": {
                "source": "bundle:hya/plan-impl-review",
                "name": "plan-impl-review",
                "revision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            }
        }),
        json!({
            "type": "workflow_run_started",
            "session": root,
            "run": wf_run,
            "workflow": {
                "source": "bundle:hya/plan-impl-review",
                "name": "plan-impl-review",
                "revision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            },
            "request_hash": "abc123",
            "owner": active_owner,
            "stages": [{
                "id": "plan",
                "title": "Plan",
                "agent": "hya-task",
                "mode": "once",
                "level": 0
            }]
        }),
        json!({
            "type": "workflow_stage_started",
            "session": root,
            "run": wf_run,
            "stage": "plan"
        }),
        json!({
            "type": "workflow_stage_member_linked",
            "session": root,
            "run": wf_run,
            "stage": "plan",
            "member": member_id,
            "role": "worker",
            "iteration": 0
        }),
        json!({
            "type": "workflow_stage_finished",
            "session": root,
            "run": wf_run,
            "stage": "plan",
            "status": "completed"
        }),
        json!({
            "type": "workflow_run_finished",
            "session": root,
            "run": wf_run,
            "status": "completed",
            "error": null
        }),
        // hya/multi-agent v1 events
        json!({
            "type": "session_status",
            "session": root,
            "status": {"busy": false}
        }),
        json!({
            "type": "member_spawned",
            "session": root,
            "member": member_id,
            "child": child,
            "subagent_type": AgentName::new("hya-task"),
            "description": "implement feature",
            "depth": 1,
            "directive": "build it",
            "tool_call": tool_call
        }),
        json!({
            "type": "member_status_changed",
            "session": root,
            "member": member_id,
            "status": "running"
        }),
        json!({
            "type": "agent_registered",
            "session": root,
            "agent_session": root,
            "handle": "main",
            "agent_type": AgentName::new("hya-main"),
            "mode": "transient"
        }),
        json!({
            "type": "agent_registered",
            "session": root,
            "agent_session": child,
            "handle": "worker-1",
            "parent": "main",
            "agent_type": AgentName::new("hya-task"),
            "mode": "resident"
        }),
        json!({
            "type": "agent_activity_changed",
            "session": root,
            "handle": "main/worker-1",
            "status": "working",
            "current_task": "writing tests"
        }),
        json!({
            "type": "resident_work_started",
            "session": root,
            "actor_session": child,
            "handle": "main/worker-1",
            "epoch": 3,
            "inbox_through": 1
        }),
        json!({
            "type": "agent_heartbeat",
            "session": root,
            "handle": "main/worker-1",
            "heartbeat_ms": 1700000000000u64
        }),
        json!({
            "type": "resident_work_finished",
            "session": root,
            "handle": "main/worker-1",
            "epoch": 3,
            "inbox_through": 1
        }),
        // hya/agent-channels v1 events
        json!({
            "type": "channel_created",
            "session": root,
            "channel": "dm-main-worker-1",
            "kind": "dm",
            "unit": null,
            "members": ["main", "main/worker-1"]
        }),
        json!({
            "type": "channel_joined",
            "session": root,
            "channel": "dm-main-worker-1",
            "member": "main/worker-1"
        }),
        json!({
            "type": "mail_sent",
            "session": root,
            "from": "main",
            "to": {"kind": "channel", "id": "dm-main-worker-1"},
            "kind": "message",
            "body": "please verify"
        }),
        json!({
            "type": "mail_consumed",
            "session": root,
            "handle": "main/worker-1",
            "through": 1
        }),
        json!({
            "type": "subagent_reported",
            "session": root,
            "member": member_id,
            "child": child,
            "handle": "main/worker-1",
            "outcome": "done",
            "report": "all green"
        }),
        json!({
            "type": "member_finished",
            "session": root,
            "member": member_id,
            "status": "completed",
            "summary": "all green",
            "child": child
        }),
        json!({
            "type": "agent_restarted",
            "session": root,
            "handle": "main/worker-1",
            "child": child,
            "epoch": 4
        }),
        json!({
            "type": "agent_archived",
            "session": root,
            "handle": "main/worker-1",
            "child": child,
            "reason": "reported"
        }),
    ];

    let mut ts = 1000i64;
    for ev in root_events {
        sqlx::query("INSERT INTO event_log (session_id, payload, ts) VALUES (?, ?, ?)")
            .bind(root.storage_key())
            .bind(serde_json::to_string(&ev).unwrap())
            .bind(ts)
            .execute(&mut conn)
            .await
            .unwrap();
        ts += 10;
    }

    // Child session events including `handoff_committed`.
    let child_events: Vec<JsonValue> = vec![
        json!({
            "type": "session_created",
            "session": child,
            "parent": root,
            "agent": "hya-task",
            "model": "hya/offline",
            "workdir": "/work/demo",
            "project_id": project_id,
            "kind": "project"
        }),
        json!({
            "type": "handoff_committed",
            "session": child,
            "handle": "main/worker-1",
            "generation": 2,
            "doc": "1. Goal\nFinished migration verification.",
            "degraded": false
        }),
    ];
    for ev in child_events {
        sqlx::query("INSERT INTO event_log (session_id, payload, ts) VALUES (?, ?, ?)")
            .bind(child.storage_key())
            .bind(serde_json::to_string(&ev).unwrap())
            .bind(ts)
            .execute(&mut conn)
            .await
            .unwrap();
        ts += 10;
    }

    conn.close().await.unwrap();
    (root, child, active_owner, tool_part)
}

async fn seed_all_v1_fixtures(root: &Path) -> SeededV1Fixtures {
    let config_dir = root.join("config");
    let state_dir = root.join("state");
    let data_dir = root.join("data");
    let bundles_dir = root.join("project/.hya/bundles");
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&bundles_dir).unwrap();

    let db_path = state_dir.join("sessions.db");
    let (root_session, child_session, active_owner, tool_part) = seed_v1_database(&db_path).await;

    // 2. Realistic v1 config.yaml
    let config_path = config_dir.join("config.yaml");
    let v1_config = r#"default_model: hya/offline
providers: {}
mcp: {}
plugins: {}
bundles:
  builtin:
    profile: full
    enable:
      - base-tools
      - plan-impl-review
      - code-mode
    disable:
      - network-tools
      - goal-loop
      - channel-tools
tools:
  code:
    mode: with_tools
  websearch:
    enabled: true
    provider: exa
  webfetch:
    enabled: false
lsp:
  rust:
    command: ["rust-analyzer"]
formatter:
  rustfmt:
    command: ["rustfmt"]
subagents:
  max_concurrency: 6
  per_run_budget: 24
goal:
  evaluator_model: openai/gpt-5
compaction:
  keep_recent: 8
  summary_max_tokens: 1500
  token_accounting: estimate
permission:
  model: default
  rules: []
"#;
    fs::write(&config_path, v1_config).unwrap();

    // 3. Realistic v1 tui.toml
    let tui_toml_path = config_dir.join("tui.toml");
    let v1_tui_toml = r#"version = 1
preset = "workspace"

[subagents]
enabled = true

[workflows]
enabled = true

[presets.dev]
extends = "dock"

[presets.dev.subagents]
enabled = false

[presets.dev.layout]
panes = ["conversation", "subagents", "workflows"]
"#;
    fs::write(&tui_toml_path, v1_tui_toml).unwrap();

    // 4. Realistic v1 tui.json (legacy v3 binary paneLayout + legacy extension bundle ids)
    let tui_json_path = config_dir.join("tui.json");
    let v1_tui_json = json!({
        "theme": "hya",
        "vim": true,
        "extensionEnabled": {
            "hya/channel-tools": true,
            "hya/plan-impl-review": false,
            "hya/code-mode": true
        },
        "extensionTrusted": {
            "network-tools": true
        },
        "paneLayout": {
            "version": 3,
            "active": "pane-1",
            "root": {
                "type": "split",
                "axis": "vertical",
                "weight": 0.7,
                "first": {
                    "type": "split",
                    "axis": "horizontal",
                    "weight": 0.8,
                    "first": { "type": "pane", "id": "pane-6", "kind": "conversation" },
                    "second": {
                        "type": "split",
                        "axis": "horizontal",
                        "weight": 0.2,
                        "first": { "type": "pane", "id": "pane-7", "kind": "activity" },
                        "second": { "type": "pane", "id": "pane-1", "kind": "composer" }
                    }
                },
                "second": {
                    "type": "split",
                    "axis": "horizontal",
                    "weight": 0.5,
                    "first": { "type": "pane", "id": "pane-2", "kind": "subagents" },
                    "second": {
                        "type": "split",
                        "axis": "horizontal",
                        "weight": 0.5,
                        "first": { "type": "pane", "id": "pane-3", "kind": "subagent-viewer", "session": child_session.to_string() },
                        "second": { "type": "pane", "id": "pane-4", "kind": "workflows" }
                    }
                }
            }
        }
    });
    fs::write(
        &tui_json_path,
        format!("{}\n", serde_json::to_string_pretty(&v1_tui_json).unwrap()),
    )
    .unwrap();

    // 5. Realistic v1 user bundles under `.hya/bundles/`
    let js_bundle_dir = bundles_dir.join("acme-js");
    fs::create_dir_all(js_bundle_dir.join("ext")).unwrap();
    fs::create_dir_all(js_bundle_dir.join("data")).unwrap();
    fs::write(
        js_bundle_dir.join("bundle.yaml"),
        r#"kind: Bundle
schema_version: 1
version_ref: backend
identity:
  id: acme/js-helper
  version: 0.0.0
  publisher: acme
extensions:
  js:
    - id: entry
      path: ext/index.ts
  files:
    - id: notes
      path: data/notes.txt
    - id: exposure
      path: exposure.yaml
"#,
    )
    .unwrap();
    fs::write(js_bundle_dir.join("ext/index.ts"), "export default {}\n").unwrap();
    fs::write(js_bundle_dir.join("data/notes.txt"), "hello asset\n").unwrap();
    fs::write(js_bundle_dir.join("exposure.yaml"), "scheme: legacy\n").unwrap();

    let md_bundle_dir = bundles_dir.join("acme-md");
    fs::create_dir_all(&md_bundle_dir).unwrap();
    fs::write(
        md_bundle_dir.join("bundle.hya.md"),
        r#"---
kind: Bundle
identity:
  id: acme/md-agent
  version: 1.0.0
  publisher: acme
agents:
  - id: acme-reviewer
    description: Markdown bundle agent
    role: subagent
---
Review the diff carefully.
"#,
    )
    .unwrap();

    let reserved_bundle_dir = bundles_dir.join("acme-reserved");
    fs::create_dir_all(reserved_bundle_dir.join("tools")).unwrap();
    fs::write(
        reserved_bundle_dir.join("bundle.yaml"),
        r#"kind: Bundle
schema_version: 1
identity:
  id: acme/reserved-tools
  version: 1.0.0
  publisher: acme
resources:
  tools:
    - id: todo
      path: tools/todo.yaml
"#,
    )
    .unwrap();
    fs::write(
        reserved_bundle_dir.join("tools/todo.yaml"),
        "description: bad override\n",
    )
    .unwrap();

    // 6. Realistic v1 `registry.sqlite3` with a v1 prepared catalog row containing `exposure.yaml`
    //    and legacy disabled bundle IDs.
    let registry_path = data_dir.join("registry.sqlite3");
    {
        let registry = BundleRegistry::connect(&registry_path.to_string_lossy())
            .await
            .unwrap();
        let valid_v2 = prepare_package(BundleSource::new(
            "acme/installed",
            vec![SourceFile::new(
                "bundle.yaml",
                "kind: Bundle\nschema_version: 2\nidentity: { id: acme/installed, version: 1.0.0, publisher: acme }\n",
            )],
        ))
        .unwrap();
        registry
            .install(
                &[],
                NamespaceInstallPolicy::DenyConflicts,
                BundleInstallCandidate {
                    source_digest: [7u8; 32],
                    prepared_digest: valid_v2.digest().to_owned(),
                    prepared_bytes: valid_v2.bytes().to_vec(),
                    installed_at: 1000,
                },
            )
            .await
            .unwrap();
        registry
            .set_bundle_enabled("hya/network-tools", false)
            .await
            .unwrap();
        registry
            .set_bundle_enabled("hya/goal-loop", false)
            .await
            .unwrap();
        registry
            .set_bundle_enabled("hya/code-mode", false)
            .await
            .unwrap();
    }
    // Mutate the installed row into a simulated legacy v1 prepared payload (`format_version: 3`,
    // which fails strict v2 PreparedCatalog::decode until `hya-migration` upgrades it).
    {
        let opts = SqliteConnectOptions::new()
            .filename(&registry_path)
            .create_if_missing(false);
        let mut conn = SqliteConnection::connect_with(&opts).await.unwrap();
        let raw_bytes: Vec<u8> = sqlx::query_scalar(
            "SELECT prepared_bytes FROM installed_bundle WHERE bundle_id = 'acme/installed'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        let mut doc: JsonValue = serde_json::from_slice(&raw_bytes).unwrap();
        doc["format_version"] = json!(3);
        let mutated = serde_json::to_vec(&doc).unwrap();
        let mutated_digest = sha256_hex(&mutated);
        sqlx::query("UPDATE installed_bundle SET prepared_bytes = ?, prepared_digest = ? WHERE bundle_id = 'acme/installed'")
            .bind(&mutated)
            .bind(&mutated_digest)
            .execute(&mut conn)
            .await
            .unwrap();
        let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&mut conn)
            .await;
        let _ = sqlx::query("PRAGMA journal_mode = DELETE")
            .execute(&mut conn)
            .await;
        conn.close().await.unwrap();
        let _ = fs::remove_file(format!("{}-wal", registry_path.display()));
        let _ = fs::remove_file(format!("{}-shm", registry_path.display()));
    }

    SeededV1Fixtures {
        targets: MigrationTargets {
            db: db_path,
            config: config_path,
            tui_toml: tui_toml_path,
            tui_json: tui_json_path,
            registry: registry_path,
            bundles_dirs: vec![bundles_dir],
        },
        root_session,
        child_session,
        active_owner,
        tool_part,
        js_bundle_dir,
        md_bundle_dir,
        reserved_bundle_dir,
    }
}

fn snapshot_hashes(paths: &[&Path]) -> BTreeMap<PathBuf, String> {
    paths
        .iter()
        .map(|p| ((*p).to_path_buf(), file_sha256(p).unwrap().0))
        .collect()
}

#[tokio::test]
async fn full_v1_to_v2_migration_v2_runtime_verification_and_sha256_rollback() {
    let ws = TempWorkspace::new("full");
    let seeded = seed_all_v1_fixtures(&ws.root).await;

    let js_manifest = seeded.js_bundle_dir.join("bundle.yaml");
    let js_exposure = seeded.js_bundle_dir.join("exposure.yaml");
    let md_manifest = seeded.md_bundle_dir.join("bundle.hya.md");
    let reserved_manifest = seeded.reserved_bundle_dir.join("bundle.yaml");

    let tracked_originals: Vec<&Path> = vec![
        &seeded.targets.db,
        &seeded.targets.config,
        &seeded.targets.tui_toml,
        &seeded.targets.tui_json,
        &seeded.targets.registry,
        &js_manifest,
        &js_exposure,
        &md_manifest,
        &reserved_manifest,
    ];
    let pre_hashes = snapshot_hashes(&tracked_originals);

    // -----------------------------------------------------------------------
    // 1. `check` — read-only inspection detects every v1 artifact
    // -----------------------------------------------------------------------
    let check_before = check_all(&seeded.targets).await.unwrap();
    assert!(check_before.needs_migration);
    assert_eq!(check_before.db.status, DbCheckStatus::NeedsMigration);
    assert!(check_before.db.has_resident_actor_claim);
    assert_eq!(check_before.db.resident_claim_rows, 2);
    assert_eq!(check_before.db.legacy_domain_events, 26);
    assert_eq!(
        check_before.config.status,
        ConfigCheckStatus::NeedsMigration
    );
    assert_eq!(
        check_before.tui.tui_toml_status,
        TuiCheckStatus::NeedsMigration
    );
    assert_eq!(
        check_before.tui.tui_json_status,
        TuiCheckStatus::NeedsMigration
    );
    assert_eq!(
        check_before.bundles.registry.status,
        RegistryCheckStatus::NeedsMigration
    );
    assert_eq!(check_before.bundles.registry.unreadable_v1_rows, 1);
    assert_eq!(check_before.bundles.registry.legacy_disabled_ids, 3);
    assert_eq!(check_before.bundles.manifests.len(), 3);
    assert!(
        check_before
            .bundles
            .manifests
            .iter()
            .all(|m| m.status == BundleCheckStatus::NeedsMigration)
    );
    // Reserved tool `todo` must emit an Error diagnostic.
    let reserved_check = check_before
        .bundles
        .manifests
        .iter()
        .find(|m| m.manifest_path == reserved_manifest)
        .unwrap();
    assert!(
        reserved_check
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Error && d.message.contains("todo"))
    );
    // `exposure.yaml` must emit a Warning diagnostic.
    let js_check = check_before
        .bundles
        .manifests
        .iter()
        .find(|m| m.manifest_path == js_manifest)
        .unwrap();
    assert!(
        js_check
            .diagnostics
            .iter()
            .any(|d| d.severity == DiagnosticSeverity::Warning
                && d.message.contains("exposure.yaml"))
    );

    // Verify `check` wrote NOTHING.
    assert_eq!(snapshot_hashes(&tracked_originals), pre_hashes);

    // Confirm v2 `hya-store` rejects the unmigrated v1 database!
    assert!(
        SessionStore::connect_read_only(&seeded.targets.db.to_string_lossy())
            .await
            .is_err(),
        "unmigrated v1 DB must fail fast in v2 hya-store"
    );

    // -----------------------------------------------------------------------
    // 2. `run --dry-run` — simulates everything without touching disk
    // -----------------------------------------------------------------------
    let backup_dir = ws.root.join("backup-snapshot");
    let dry_report = run_migration(
        &seeded.targets,
        &RunOptions {
            dry_run: true,
            backup_dir: Some(backup_dir.clone()),
        },
    )
    .await
    .unwrap();
    assert!(dry_report.dry_run);
    assert!(dry_report.backup_dir.is_none());
    assert!(!backup_dir.exists());
    assert!(!dry_report.db.migrated);
    assert_eq!(dry_report.db.events_rewritten, 26);
    assert_eq!(dry_report.db.claims_migrated, 2);
    assert!(!dry_report.config.migrated);
    assert_eq!(dry_report.config.bundle_configs_written.len(), 5);
    assert_eq!(snapshot_hashes(&tracked_originals), pre_hashes);
    assert!(!user_bundle_config_path(&seeded.targets.config, "hya/network").exists());

    // -----------------------------------------------------------------------
    // 3. `run` — writes SHA-256 backup manifest, then migrates atomically
    // -----------------------------------------------------------------------
    let run_report = run_migration(
        &seeded.targets,
        &RunOptions {
            dry_run: false,
            backup_dir: Some(backup_dir.clone()),
        },
    )
    .await
    .unwrap();
    assert!(!run_report.dry_run);
    assert_eq!(run_report.backup_dir.as_deref(), Some(backup_dir.as_path()));
    assert!(run_report.db.migrated);
    assert_eq!(run_report.db.events_rewritten, 26);
    assert_eq!(run_report.db.claims_migrated, 2);
    assert!(run_report.db.integrity_ok);
    assert!(run_report.db.store_verified);
    assert!(run_report.config.migrated);
    assert!(run_report.tui.tui_toml_migrated);
    assert!(run_report.tui.tui_json_migrated);
    assert!(run_report.bundles.registry_migrated);
    assert_eq!(run_report.bundles.registry_rows_upgraded, 1);
    assert_eq!(run_report.bundles.registry_disabled_remapped, 3);
    assert_eq!(run_report.bundles.manifests_migrated.len(), 3);

    let verified_manifest = verify_backup_manifest(&backup_dir).unwrap();
    assert!(verified_manifest.entries.iter().any(|e| e.existed));
    assert!(verified_manifest.entries.iter().any(|e| !e.existed));

    // Re-running `check` after migration must show everything is now v2!
    let check_after = check_all(&seeded.targets).await.unwrap();
    assert!(
        !check_after.needs_migration,
        "expected already-v2: {check_after:#?}"
    );
    assert_eq!(check_after.db.status, DbCheckStatus::AlreadyV2);
    assert_eq!(check_after.config.status, ConfigCheckStatus::AlreadyV2);
    assert_eq!(check_after.tui.tui_toml_status, TuiCheckStatus::AlreadyV2);
    assert_eq!(check_after.tui.tui_json_status, TuiCheckStatus::AlreadyV2);
    assert_eq!(
        check_after.bundles.registry.status,
        RegistryCheckStatus::AlreadyV2
    );
    assert!(
        check_after
            .bundles
            .manifests
            .iter()
            .all(|m| m.status == BundleCheckStatus::AlreadyV2)
    );

    // -----------------------------------------------------------------------
    // 4. Verify v2 `hya-store` opens & replays all migrated sessions/claims
    // -----------------------------------------------------------------------
    {
        let store = SessionStore::connect(&seeded.targets.db.to_string_lossy())
            .await
            .unwrap();
        let reducers = projection_reducers();

        let root_envelopes = store.replay(seeded.root_session).await.unwrap();
        assert_eq!(root_envelopes.len(), 28);
        let root_proj = hya_proto::Projection::from_events_with(&root_envelopes, &reducers);

        // Compaction eviction + status
        let ctx_status = context_status_projection(&root_proj).unwrap();
        assert_eq!(ctx_status.tokens, 3000);
        assert_eq!(ctx_status.threshold, 8000);
        assert!(matches!(
            &root_proj.session.messages[0].parts[0],
            PartProjection::Tool {
                id,
                eviction_notice: Some(notice),
                ..
            } if *id == seeded.tool_part && notice == "[moved to artifact://abc]"
        ));

        // Workflow projection
        let wf = workflow_projection(&root_proj).expect("workflow projection must be present");
        assert_eq!(
            wf.selection.as_ref().map(|w| w.name.as_str()),
            Some("plan-impl-review")
        );
        assert!(wf.run.is_some());

        // Multi-agent members + roster/archive
        let members = members_projection(&root_proj);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].summary, "all green");
        let team = team_projection(&root_proj);
        assert!(team.channels.contains_key("dm-main-worker-1"));
        assert_eq!(team.mail.len(), 1);
        assert_eq!(team.mail[0].body, "please verify");
        assert!(team.archived.contains_key("main/worker-1"));

        // Child handoff projection
        let child_envelopes = store.replay(seeded.child_session).await.unwrap();
        let child_proj = hya_proto::Projection::from_events_with(&child_envelopes, &reducers);
        let handoff = handoff_projection(&child_proj).unwrap();
        assert_eq!(handoff.generation, 2);
        assert!(handoff.doc.contains("Finished migration verification."));

        // `resident_actor_claim` -> `bundle_claim` + `bundle_state`
        let active_claim = store
            .get_bundle_claim(BUNDLE_MULTI_AGENT, &seeded.child_session.to_string())
            .await
            .unwrap()
            .expect("active migrated claim must be readable via v2 bundle_claim");
        assert_eq!(active_claim.epoch, 3);
        assert_eq!(
            active_claim.owner,
            seeded.active_owner.as_uuid().to_string()
        );
        assert_eq!(
            active_claim.metadata["root_session_id"],
            json!(seeded.root_session.to_string())
        );

        let provenance = store
            .get_bundle_state_json(BUNDLE_MULTI_AGENT, "migration/v1_resident_claims")
            .await
            .unwrap()
            .expect("bundle_state provenance must be recorded");
        assert_eq!(provenance["migrated_claims"], json!(2));

        // Projects & permissions preserved
        let projects = store.list_projects().await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].project.name, "Demo");
        let _ = store
            .drain_for_exit(std::time::Duration::from_secs(2))
            .await;
    }

    // -----------------------------------------------------------------------
    // 5. Verify v2 `config.yaml` + per-bundle `config.yml` files
    // -----------------------------------------------------------------------
    {
        let raw_v2_cfg = fs::read_to_string(&seeded.targets.config).unwrap();
        let cfg_val: serde_norway::Value = serde_norway::from_str(&raw_v2_cfg).unwrap();
        assert_eq!(cfg_val["schema_version"].as_u64(), Some(2));
        assert!(cfg_val.get("compaction").is_none());
        assert!(cfg_val.get("goal").is_none());
        assert!(cfg_val.get("subagents").is_none());
        assert!(cfg_val.get("formatter").is_none());
        assert!(cfg_val.get("lsp").is_none());
        assert!(cfg_val["tools"].get("websearch").is_none());
        assert!(cfg_val["tools"].get("webfetch").is_none());
        assert_eq!(
            cfg_val["tools"]["code"]["mode"].as_str(),
            Some("with_tools")
        );

        let enable: Vec<&str> = cfg_val["bundles"]["builtin"]["enable"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(
            enable,
            vec!["hya/hashline", "hya/coding-extras", "hya/workflow"]
        );
        let disable: Vec<&str> = cfg_val["bundles"]["builtin"]["disable"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(
            disable,
            vec!["hya/network", "hya/goal", "hya/loop", "hya/agent-channels"]
        );

        let net_cfg: serde_norway::Value = serde_norway::from_str(
            &fs::read_to_string(user_bundle_config_path(
                &seeded.targets.config,
                "hya/network",
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(net_cfg["websearch"]["provider"].as_str(), Some("exa"));
        assert_eq!(net_cfg["webfetch"]["enabled"].as_bool(), Some(false));

        let coding_cfg: serde_norway::Value = serde_norway::from_str(
            &fs::read_to_string(user_bundle_config_path(
                &seeded.targets.config,
                "hya/coding-extras",
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            coding_cfg["lsp"]["rust"]["command"][0].as_str(),
            Some("rust-analyzer")
        );
        assert_eq!(
            coding_cfg["formatter"]["rustfmt"]["command"][0].as_str(),
            Some("rustfmt")
        );

        let goal_cfg: serde_norway::Value = serde_norway::from_str(
            &fs::read_to_string(user_bundle_config_path(&seeded.targets.config, "hya/goal"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            goal_cfg["goal"]["evaluator_model"].as_str(),
            Some("openai/gpt-5")
        );

        let comp_cfg: serde_norway::Value = serde_norway::from_str(
            &fs::read_to_string(user_bundle_config_path(
                &seeded.targets.config,
                "hya/compaction",
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(comp_cfg["compaction"]["keep_recent"].as_u64(), Some(8));

        let ma_cfg: serde_norway::Value = serde_norway::from_str(
            &fs::read_to_string(user_bundle_config_path(
                &seeded.targets.config,
                "hya/multi-agent",
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(ma_cfg["subagents"]["max_concurrency"].as_u64(), Some(6));

        // Verify v2 `hya-app::config::load` parses migrated `config.yaml` and reads bundle configs!
        let xdg_root = ws.root.join("xdg-check");
        fs::create_dir_all(xdg_root.join("hya")).unwrap();
        // Copy migrated config tree into xdg_root/hya
        fs::copy(&seeded.targets.config, xdg_root.join("hya/config.yaml")).unwrap();
        for bundle_id in [
            "hya/network",
            "hya/coding-extras",
            "hya/goal",
            "hya/compaction",
            "hya/multi-agent",
        ] {
            let src = user_bundle_config_path(&seeded.targets.config, bundle_id);
            let dst = user_bundle_config_path(&xdg_root.join("hya/config.yaml"), bundle_id);
            fs::create_dir_all(dst.parent().unwrap()).unwrap();
            fs::copy(&src, &dst).unwrap();
        }
        // SAFETY: isolated test verification of `XDG_CONFIG_HOME`, restored immediately.
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &xdg_root);
        }
        let loaded = hya_app::config::load().await;
        unsafe {
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
        let resolved = loaded
            .expect("v2 hya-app::config::load must accept migrated config.yaml")
            .expect("migrated config.yaml must be discovered under XDG_CONFIG_HOME");
        assert_eq!(
            resolved.settings.goal().evaluator_model.as_deref(),
            Some("openai/gpt-5")
        );
    }

    // -----------------------------------------------------------------------
    // 6. Verify v2 `tui.toml` and `tui.json`
    // -----------------------------------------------------------------------
    {
        let tui_toml_text = fs::read_to_string(&seeded.targets.tui_toml).unwrap();
        let toml_tbl: toml::Table = toml::from_str(&tui_toml_text).unwrap();
        assert_eq!(toml_tbl["version"].as_integer(), Some(2));
        assert!(toml_tbl.get("subagents").is_none());
        assert!(toml_tbl.get("workflows").is_none());
        assert_eq!(
            toml_tbl["panes"][PANEL_SUBAGENTS]["enabled"].as_bool(),
            Some(true)
        );
        assert_eq!(
            toml_tbl["panes"][PANEL_WORKFLOWS]["enabled"].as_bool(),
            Some(true)
        );
        let dev_layout = toml_tbl["presets"]["dev"]["layout"]["panes"]
            .as_array()
            .unwrap();
        assert_eq!(dev_layout[1].as_str(), Some(PANEL_SUBAGENTS));
        assert_eq!(dev_layout[2].as_str(), Some(PANEL_WORKFLOWS));

        let tui_json: JsonValue =
            serde_json::from_str(&fs::read_to_string(&seeded.targets.tui_json).unwrap()).unwrap();
        assert_eq!(tui_json["paneLayout"]["version"], json!(4));
        assert_eq!(tui_json["paneLayout"]["root"]["type"], json!("split"));
        assert_eq!(tui_json["paneLayout"]["root"]["direction"], json!("row"));

        // Collect all leaves from migrated v4 tree
        let mut panels = Vec::new();
        let mut viewer_session = None;
        fn walk_leaves(
            node: &JsonValue,
            panels: &mut Vec<(String, Option<String>)>,
            viewer_session: &mut Option<String>,
        ) {
            if node["type"] == "pane" {
                let kind = node["kind"].as_str().unwrap_or("").to_owned();
                let panel = node
                    .get("panel")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned);
                if panel.as_deref() == Some(PANEL_SUBAGENT_VIEWER) {
                    *viewer_session = node
                        .get("session")
                        .and_then(JsonValue::as_str)
                        .map(str::to_owned);
                }
                panels.push((kind, panel));
            } else if let Some(children) = node.get("children").and_then(JsonValue::as_array) {
                for c in children {
                    walk_leaves(&c["node"], panels, viewer_session);
                }
            }
        }
        walk_leaves(
            &tui_json["paneLayout"]["root"],
            &mut panels,
            &mut viewer_session,
        );
        assert!(panels.contains(&("extension".to_owned(), Some(PANEL_SUBAGENTS.to_owned()))));
        assert!(panels.contains(&(
            "extension".to_owned(),
            Some(PANEL_SUBAGENT_VIEWER.to_owned())
        )));
        assert!(panels.contains(&("extension".to_owned(), Some(PANEL_WORKFLOWS.to_owned()))));
        assert_eq!(
            viewer_session.as_deref(),
            Some(seeded.child_session.to_string().as_str())
        );

        assert_eq!(
            tui_json["extensionEnabled"]["hya/agent-channels"],
            json!(true)
        );
        assert_eq!(tui_json["extensionEnabled"]["hya/workflow"], json!(false));
        assert!(tui_json["extensionEnabled"].get("hya/code-mode").is_none());
        assert_eq!(tui_json["extensionTrusted"]["hya/network"], json!(true));
    }

    // -----------------------------------------------------------------------
    // 7. Verify v2 `hya-bundle` and `hya-app::BundleRegistry` load upgraded artifacts
    // -----------------------------------------------------------------------
    {
        let js_prepared =
            prepare_package(BundleSource::read_directory(&seeded.js_bundle_dir).unwrap())
                .expect("upgraded v2 JS bundle must prepare cleanly in hya-bundle");
        assert_eq!(js_prepared.bundles()[0].identity().id, "acme/js-helper");

        let md_prepared =
            prepare_package(BundleSource::read_directory(&seeded.md_bundle_dir).unwrap())
                .expect("upgraded v2 Markdown bundle must prepare cleanly in hya-bundle");
        assert_eq!(md_prepared.bundles()[0].identity().id, "acme/md-agent");

        let registry = BundleRegistry::connect(&seeded.targets.registry.to_string_lossy())
            .await
            .unwrap();
        let snap = registry.snapshot().await.unwrap();
        assert!(
            snap.unreadable.is_empty(),
            "migrated registry must have 0 unreadable rows"
        );
        assert_eq!(snap.bundles.len(), 1);
        assert!(
            PreparedCatalog::decode(
                &snap.bundles[0].prepared_bytes,
                &snap.bundles[0].prepared_digest
            )
            .is_ok()
        );
        let disabled = registry.disabled_bundle_ids().await.unwrap();
        assert!(disabled.contains("hya/network"));
        assert!(disabled.contains("hya/goal"));
        assert!(disabled.contains("hya/loop"));
        assert!(!disabled.contains("hya/network-tools"));
        assert!(!disabled.contains("hya/code-mode"));
    }

    // -----------------------------------------------------------------------
    // 8. SHA-256 tamper protection + byte-exact `rollback`
    // -----------------------------------------------------------------------
    let first_backup_rel = verified_manifest
        .entries
        .iter()
        .find(|e| e.existed)
        .unwrap()
        .backup_rel_path
        .as_ref()
        .unwrap();
    let tampered_file = backup_dir.join(first_backup_rel);
    let saved_backup_bytes = fs::read(&tampered_file).unwrap();
    let mut corrupted = saved_backup_bytes.clone();
    corrupted[0] ^= 0xFF;
    fs::write(&tampered_file, &corrupted).unwrap();

    assert!(
        !js_exposure.exists(),
        "legacy exposure.yaml must be removed during v2 upgrade"
    );
    let post_live: Vec<&Path> = tracked_originals
        .iter()
        .copied()
        .filter(|p| *p != js_exposure.as_path())
        .collect();
    let post_migration_hashes = snapshot_hashes(&post_live);
    let err = rollback(&backup_dir).unwrap_err();
    assert!(
        matches!(err, MigrationError::BackupVerification(_)),
        "expected BackupVerification error, got: {err}"
    );
    // Live files must be untouched when rollback verification fails!
    assert_eq!(snapshot_hashes(&post_live), post_migration_hashes);
    assert!(!js_exposure.exists());

    // Restore valid backup bytes and execute rollback!
    fs::write(&tampered_file, &saved_backup_bytes).unwrap();
    let rollback_report = rollback(&backup_dir).unwrap();
    assert_eq!(
        rollback_report.restored_files.len(),
        tracked_originals.len()
    );
    assert_eq!(rollback_report.removed_created_files.len(), 5);

    // Every original file must be byte-exact identical to pre-migration SHA-256!
    assert_eq!(snapshot_hashes(&tracked_originals), pre_hashes);
    // All newly created bundle config files must be removed!
    for bundle_id in [
        "hya/network",
        "hya/coding-extras",
        "hya/goal",
        "hya/compaction",
        "hya/multi-agent",
    ] {
        assert!(!user_bundle_config_path(&seeded.targets.config, bundle_id).exists());
    }
}

#[tokio::test]
async fn cli_binary_check_dry_run_run_and_rollback() {
    let ws = TempWorkspace::new("cli");
    let seeded = seed_all_v1_fixtures(&ws.root).await;
    let backup_dir = ws.root.join("cli-backup");
    let bin = std::env::var("NEXTEST_BIN_EXE_hya-migration")
        .or_else(|_| std::env::var("CARGO_BIN_EXE_hya-migration"))
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_hya-migration").to_owned());

    let common_args = vec![
        "--db".to_owned(),
        seeded.targets.db.to_string_lossy().into_owned(),
        "--config".to_owned(),
        seeded.targets.config.to_string_lossy().into_owned(),
        "--tui-toml".to_owned(),
        seeded.targets.tui_toml.to_string_lossy().into_owned(),
        "--tui-json".to_owned(),
        seeded.targets.tui_json.to_string_lossy().into_owned(),
        "--registry".to_owned(),
        seeded.targets.registry.to_string_lossy().into_owned(),
        "--bundles-dir".to_owned(),
        seeded.targets.bundles_dirs[0]
            .to_string_lossy()
            .into_owned(),
    ];

    let pre_db_sha = file_sha256(&seeded.targets.db).unwrap().0;
    let pre_cfg_sha = file_sha256(&seeded.targets.config).unwrap().0;

    // 1. check --json
    let out = Command::new(&bin)
        .arg("--json")
        .arg("check")
        .args(&common_args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "check failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let check_json: JsonValue = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(check_json["needs_migration"], json!(true));

    // 2. run --dry-run --json
    let out = Command::new(&bin)
        .arg("--json")
        .arg("run")
        .arg("--dry-run")
        .args(&common_args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "dry-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(file_sha256(&seeded.targets.db).unwrap().0, pre_db_sha);
    assert_eq!(file_sha256(&seeded.targets.config).unwrap().0, pre_cfg_sha);

    // 3. run --backup-dir <dir> --json
    let out = Command::new(&bin)
        .arg("--json")
        .arg("run")
        .arg("--backup-dir")
        .arg(&backup_dir)
        .args(&common_args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run_json: JsonValue = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(run_json["db"]["migrated"], json!(true));
    assert_eq!(run_json["db"]["store_verified"], json!(true));

    // 4. rollback --backup <dir> --json
    let out = Command::new(&bin)
        .arg("--json")
        .arg("rollback")
        .arg("--backup")
        .arg(&backup_dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rollback failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rb_json: JsonValue = serde_json::from_slice(&out.stdout).unwrap();
    assert!(rb_json["verified_files"].as_u64().unwrap() >= 8);
    assert_eq!(file_sha256(&seeded.targets.db).unwrap().0, pre_db_sha);
    assert_eq!(file_sha256(&seeded.targets.config).unwrap().0, pre_cfg_sha);
}

#[test]
fn all_26_v1_domain_event_tags_rewrite_into_valid_v2_extensions() {
    use hya_migration::db_v1_to_v2::{legacy_event_bundle, rewrite_event_payload};

    let session = SessionId::new();
    let tags = [
        ("context_evicted", "hya/compaction"),
        ("context_status", "hya/compaction"),
        ("workflow_selected", "hya/workflow"),
        ("workflow_run_started", "hya/workflow"),
        ("workflow_stage_started", "hya/workflow"),
        ("workflow_stage_member_linked", "hya/workflow"),
        ("workflow_stage_route_outcome", "hya/workflow"),
        ("workflow_stage_finished", "hya/workflow"),
        ("workflow_run_finished", "hya/workflow"),
        ("mail_sent", "hya/agent-channels"),
        ("channel_joined", "hya/agent-channels"),
        ("channel_created", "hya/agent-channels"),
        ("mail_consumed", "hya/agent-channels"),
        ("session_status", "hya/multi-agent"),
        ("member_spawned", "hya/multi-agent"),
        ("member_status_changed", "hya/multi-agent"),
        ("member_finished", "hya/multi-agent"),
        ("agent_registered", "hya/multi-agent"),
        ("agent_activity_changed", "hya/multi-agent"),
        ("resident_work_started", "hya/multi-agent"),
        ("resident_work_finished", "hya/multi-agent"),
        ("agent_heartbeat", "hya/multi-agent"),
        ("subagent_reported", "hya/multi-agent"),
        ("handoff_committed", "hya/multi-agent"),
        ("agent_archived", "hya/multi-agent"),
        ("agent_restarted", "hya/multi-agent"),
    ];
    for (tag, expected_bundle) in tags {
        assert_eq!(legacy_event_bundle(tag), Some(expected_bundle));
        let raw = json!({
            "type": tag,
            "session": session,
            "extra": 42
        })
        .to_string();
        let (rewritten, matched) = rewrite_event_payload(&raw, &session.storage_key())
            .unwrap()
            .expect("v1 domain event must rewrite");
        assert_eq!(matched, tag);
        let decoded: hya_proto::Event = serde_json::from_str(&rewritten).unwrap();
        match decoded {
            hya_proto::Event::Extension {
                session: got_session,
                bundle,
                kind,
                payload,
            } => {
                assert_eq!(got_session, session);
                assert_eq!(bundle, expected_bundle);
                assert_eq!(kind, tag);
                assert_eq!(payload["extra"], json!(42));
            }
            other => panic!("expected Event::Extension for `{tag}`, got {other:?}"),
        }
    }

    // Kernel events must stay untouched.
    let kernel = json!({
        "type": "session_titled",
        "session": session,
        "title": "Keep Me"
    })
    .to_string();
    assert!(
        rewrite_event_payload(&kernel, &session.storage_key())
            .unwrap()
            .is_none()
    );
}

#[test]
fn config_migration_merges_with_existing_bundle_config_and_rollback_restores_both() {
    use hya_migration::backup::{create_backup_snapshot, rollback_from_backup};
    use hya_migration::config_v1_to_v2::{apply_config_plan, plan_config_migration};

    let ws = TempWorkspace::new("cfg-merge");
    let cfg_path = ws.root.join("config.yaml");
    fs::write(
        &cfg_path,
        "default_model: hya/offline\nwebsearch:\n  provider: tavily\n",
    )
    .unwrap();
    let existing_net = user_bundle_config_path(&cfg_path, "hya/network");
    fs::create_dir_all(existing_net.parent().unwrap()).unwrap();
    let original_net_yaml = "custom_key: preserve_me\n";
    fs::write(&existing_net, original_net_yaml).unwrap();

    let pre_net_sha = file_sha256(&existing_net).unwrap().0;
    let plan = plan_config_migration(&cfg_path).unwrap();
    assert!(plan.needs_migration);

    let backup_dir = ws.root.join("backup");
    create_backup_snapshot(&backup_dir, &plan.backup_targets()).unwrap();
    let report = apply_config_plan(&plan, false).unwrap();
    assert!(report.migrated);

    let merged: serde_norway::Value =
        serde_norway::from_str(&fs::read_to_string(&existing_net).unwrap()).unwrap();
    assert_eq!(merged["custom_key"].as_str(), Some("preserve_me"));
    assert_eq!(merged["websearch"]["provider"].as_str(), Some("tavily"));

    rollback_from_backup(&backup_dir).unwrap();
    assert_eq!(file_sha256(&existing_net).unwrap().0, pre_net_sha);
    assert_eq!(
        fs::read_to_string(&existing_net).unwrap(),
        original_net_yaml
    );
}

#[test]
fn tui_v1_binary_layout_upgrades_to_v4_with_dynamic_extension_panes() {
    use hya_migration::tui_v1_to_v2::migrate_pane_layout_value;

    let mut v1_layout = json!({
        "version": 1,
        "active": "pane-1",
        "root": {
            "type": "split",
            "axis": "vertical",
            "weight": 0.7,
            "first": { "type": "pane", "id": "pane-1", "kind": "conversation" },
            "second": { "type": "pane", "id": "pane-2", "kind": "workflows" }
        }
    });
    let mut changes = Vec::new();
    let changed =
        migrate_pane_layout_value(&mut v1_layout, Path::new("tui.json"), &mut changes).unwrap();
    assert!(changed);
    assert_eq!(v1_layout["version"], json!(4));
    let dumped = serde_json::to_string(&v1_layout).unwrap();
    assert!(dumped.contains(PANEL_WORKFLOWS));
    assert!(dumped.contains("\"kind\":\"composer\""));
    assert!(dumped.contains("\"kind\":\"projects\""));
}
