//! T0.1, T1.2–T1.5, T1.9, T1.11–T1.15, T1.24 — core session journey:
//! - backend boot, AGENTS.md guidance, background session titling (once),
//! - multi-round tool loop (`write`, `read`, `edit`, `bash`, `todo`, `webfetch`, `skill`),
//! - multi-session listing/resume, `/v2/sessions/{id}/messages`, `/compact`, `/summarize`,
//! - daemon `serve stop`/`serve start` interrupted-turn & permission-cancel restore + DB URI rejection,
//! - `hya exec --bare` todo, stdin `-`, `--json` JSONL stream, and `tail-session` parity.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Output;
use std::time::Duration;

use hya_api::v2 as pb;
use hya_e2e::{
    E2eEnvBuilder, HeadlessEnv, default_backend_bin, fake_requests_from, load_raw_events,
    text_step, tool_step, unique_temp_dir, wait_until,
};
use hya_proto::{Envelope, Event, FinishCause, Role};
use serde_json::{Value, json};

const AGENTS_MARKER: &str = "E2E_AGENTS_CONTEXT_MARKER_use_spaces";
const SKILL_MD: &str = r#"---
name: e2e-skill
description: Process E2E skill fixture
---
E2E_SKILL_BODY_MARKER for agent suite.
"#;

fn collect_text(events: &[Envelope]) -> String {
    let mut text = String::new();
    for env_evt in events {
        match &env_evt.event {
            Event::TextDelta { delta, .. } => text.push_str(delta),
            Event::TextReplace { text: t, .. } => text.push_str(t),
            _ => {}
        }
    }
    text
}

/// T0.1, T1.2–T1.5, T1.9, T1.11–T1.15, T1.24 on one booted backend.
#[tokio::test]
async fn core_session_tools_skills_context_and_compact() {
    const FETCH_BODY: &str = "hya-native-network-tool";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fetch_url = format!("http://{}/doc", listener.local_addr().unwrap());
    let app = axum::Router::new().route("/doc", axum::routing::get(|| async { FETCH_BODY }));
    let fetch_server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let env = E2eEnvBuilder::new()
        .project_file(
            "AGENTS.md",
            format!("# Project agents\n\n{AGENTS_MARKER}\n").into_bytes(),
        )
        .project_file("edit-me.txt", b"hello OLD_TOKEN world\n".to_vec())
        .skill_file(".hya/skills/e2e-skill/SKILL.md", SKILL_MD)
        .build()
        .await
        .expect("e2e env");
    env.fake
        .set_title_reply("Widget audit")
        .expect("title reply");

    // T0.1: backend URL responds.
    let health = reqwest::get(format!("{}/v2/health", env.backend.url))
        .await
        .expect("health request");
    assert!(
        health.status().is_success(),
        "server should respond on {}",
        env.backend.url
    );

    // T1.2 + T1.13 + T1.11 (background titling once).
    env.fake
        .push_scripts([text_step("E2E_HELLO")])
        .expect("push turn 1");
    let session_a = env.create_session().await.expect("session a");
    let turn1 = env
        .prompt_and_wait_for_fake_request(
            session_a,
            "say hello about ALPHA_TOPIC",
            Duration::from_secs(30),
        )
        .await
        .expect("turn 1");
    assert_eq!(turn1.state, pb::TurnState::Finished as i32);

    let events_a1 = env.events(session_a, None).await.expect("events a1");
    assert!(
        collect_text(&events_a1).contains("E2E_HELLO"),
        "assistant text E2E_HELLO missing; {}",
        env.diagnostics()
    );
    let req0 = env.fake.requests().unwrap()[0].to_string();
    assert!(
        req0.contains(AGENTS_MARKER)
            && (req0.contains("Project context") || req0.contains("AGENTS.md")),
        "model request must include AGENTS.md guidance: {req0}"
    );

    // Poll until background hya-title agent titles session A.
    let session_url = format!("{}/v2/sessions/{session_a}", env.backend.url);
    let mut title = Value::Null;
    for _ in 0..200 {
        let info: Value = reqwest::get(&session_url)
            .await
            .expect("get session")
            .json()
            .await
            .expect("session json");
        title = info["title"].clone();
        if !title.is_null() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(title, json!("Widget audit"), "{}", env.diagnostics());

    // T1.9: /v2/skills lists the project skill before the tool call.
    let skills = env
        .get_json(&format!(
            "/v2/skills?directory={}",
            env.backend.workdir_str()
        ))
        .await
        .expect("skill list");
    assert!(
        skills["skills"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|s| s["name"] == "e2e-skill")),
        "project skill must appear in /v2/skills: {skills}"
    );

    // T1.3–T1.5, T1.9, T1.15, T1.24: single multi-round tool loop on session A.
    let before_tools = env.fake.requests().unwrap().len();
    let expected_read_input = json!({
        "filePath": "e2e-note.txt",
        "path": "e2e-note.txt",
        "offset": 0,
        "limit": 2000
    });
    env.fake
        .push_scripts([
            tool_step(
                "write",
                json!({ "path": "e2e-note.txt", "content": "hya-e2e-fs" }),
            ),
            tool_step("read", expected_read_input.clone()),
            tool_step(
                "edit",
                json!({
                    "path": "edit-me.txt",
                    "edits": [{ "op": "replace_text", "oldText": "OLD_TOKEN", "newText": "NEW_TOKEN" }]
                }),
            ),
            tool_step(
                "bash",
                json!({ "command": "printf shell-ok > e2e-shell.txt" }),
            ),
            tool_step(
                "todo",
                json!({
                    "ops": [
                        { "op": "add", "content": "E2E_TODO_ITEM" },
                        { "op": "add", "content": "E2E_SECOND_ITEM" }
                    ]
                }),
            ),
            tool_step(
                "todo",
                json!({ "ops": [{ "op": "status", "id": "1", "status": "in_progress" }] }),
            ),
            tool_step("webfetch", json!({ "url": fetch_url, "format": "text" })),
            tool_step("skill", json!({ "name": "e2e-skill" })),
            text_step("TOOLS_AND_SKILL_DONE"),
        ])
        .expect("push tool loop");
    let tool_turn = env
        .prompt(session_a, "run write/read/edit/bash/todo/webfetch/skill")
        .await;
    fetch_server.abort();
    tool_turn.expect("tool loop turn");

    // Disk side-effect oracles (T1.4, T1.5, T1.15).
    assert_eq!(env.read_project_file("e2e-note.txt").unwrap(), "hya-e2e-fs");
    assert_eq!(env.read_project_file("e2e-shell.txt").unwrap(), "shell-ok");
    let edited = env.read_project_file("edit-me.txt").unwrap();
    assert!(
        edited.contains("NEW_TOKEN") && !edited.contains("OLD_TOKEN"),
        "edit must replace token: {edited:?}"
    );

    // Correlated tool events (T1.3 read, T1.24 webfetch) and final text.
    let events_a = env.events(session_a, None).await.expect("events a");
    let has_read_result = events_a.iter().any(|res| {
        let Event::ToolResult { call: rc, .. } = &res.event else {
            return false;
        };
        events_a.iter().any(|req| {
            matches!(
                &req.event,
                Event::ToolCallRequested { call, name, input, .. }
                    if call == rc && name.as_str() == "read" && input == &expected_read_input
            )
        })
    });
    assert!(has_read_result, "correlated read ToolResult missing");
    let fetch_call = events_a
        .iter()
        .find_map(|e| match &e.event {
            Event::ToolCallRequested { call, name, .. } if name.as_str() == "webfetch" => {
                Some(*call)
            }
            _ => None,
        })
        .expect("webfetch call");
    assert!(events_a.iter().any(|e| matches!(
        &e.event,
        Event::ToolResult { call, output, .. }
            if *call == fetch_call && output["status"] == 200 && output["output"] == FETCH_BODY
    )));
    assert!(collect_text(&events_a).contains("TOOLS_AND_SKILL_DONE"));

    // T1.9: skill body reached the follow-up FakeLlm request (not just args).
    let requests = env.fake.requests().unwrap();
    let follow_up = fake_requests_from(&requests, before_tools + 1);
    assert!(
        follow_up.contains("E2E_SKILL_BODY_MARKER"),
        "skill body missing from follow-up: {follow_up}"
    );

    // T1.15: session todo projection.
    let todos = env.session_todos(&session_a).await.expect("todos");
    let todos_blob = todos.to_string();
    assert!(
        todos_blob.contains("E2E_TODO_ITEM")
            && todos_blob.contains("E2E_SECOND_ITEM")
            && (todos_blob.contains("IN_PROGRESS") || todos_blob.contains("in_progress"))
            && (todos_blob.contains("\"id\":\"1\"") || todos_blob.contains("\"id\": \"1\"")),
        "todo route missing expected items/status/id: {todos}"
    );

    // Background titling ran only once for session A.
    assert_eq!(env.fake.title_requests().unwrap().len(), 1);
    let titled_count = events_a
        .iter()
        .filter(|e| matches!(e.event, Event::SessionTitled { .. }))
        .count();
    assert_eq!(titled_count, 1);

    // T1.11 + T1.12: second session + list_sessions + /v2/sessions/{id}/messages projection.
    let session_b = env.create_session().await.expect("session b");
    assert_ne!(session_a, session_b);
    env.fake
        .push_scripts([text_step("SESSION_B_ONE"), text_step("SESSION_B_TWO")])
        .expect("push b");
    env.prompt(session_b, "summarize detail A")
        .await
        .expect("b1");
    env.prompt(session_b, "summarize detail B")
        .await
        .expect("b2");

    let listed = env.list_sessions().await.expect("list sessions");
    assert!(
        listed
            .sessions
            .iter()
            .any(|r| r.id == session_a.to_string())
            && listed
                .sessions
                .iter()
                .any(|r| r.id == session_b.to_string()),
        "both sessions must appear in list: {listed:?}"
    );

    let ctx_a = env.session_context(&session_a).await.expect("context a");
    let ctx_blob = ctx_a.to_string();
    assert!(
        ctx_blob.contains("ALPHA_TOPIC")
            && ctx_blob.contains("E2E_HELLO")
            && ctx_blob.contains("TOOLS_AND_SKILL_DONE")
            && ctx_blob.contains("ROLE_USER")
            && ctx_blob.contains("ROLE_ASSISTANT"),
        "context projection incomplete: {ctx_a}"
    );

    // T1.14: compact session A, verify rewrite marker & eviction on follow-up turn.
    env.compact_session(&session_a)
        .await
        .expect("compact session a");
    let compacted = env
        .session_context(&session_a)
        .await
        .expect("compacted context");
    let compacted_blob = compacted.to_string();
    assert!(
        compacted_blob.contains("Earlier context was removed by the harness")
            && compacted_blob.contains("ALPHA_TOPIC"),
        "compact must record rewrite marker with removed user prompt: {compacted}"
    );
    env.fake
        .push_scripts([text_step("AFTER_COMPACT_OK")])
        .expect("push post-compact");
    env.prompt(session_a, "continue after compaction")
        .await
        .expect("post-compact turn");
    let post_events = env.events(session_a, None).await.expect("post events");
    assert!(collect_text(&post_events).contains("AFTER_COMPACT_OK"));
    let last_req = env.fake.requests().unwrap().last().unwrap().to_string();
    assert!(
        last_req.contains("Earlier context was removed by the harness")
            && !last_req.contains("E2E_HELLO"),
        "post-compact request must carry marker instead of turn 1: {last_req}"
    );

    // T1.14: /v2/sessions/{id}/summarize path on session B.
    let sum_result = env
        .post_json(&format!("/v2/sessions/{session_b}/summarize"), &Value::Null)
        .await
        .expect("summarize");
    assert!(
        sum_result
            .get("summaryMessage")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
        "summarize must return summaryMessage id: {sum_result}"
    );
    let sum_ctx = env
        .session_context(&session_b)
        .await
        .expect("summarized context")
        .to_string();
    assert!(
        sum_ctx.contains("Earlier context was removed by the harness")
            && sum_ctx.contains("summarize detail A"),
        "summarize must add rewrite marker: {sum_ctx}"
    );
}

struct RestartDaemon<'a>(&'a hya_e2e::BackendProcess);

impl Drop for RestartDaemon<'_> {
    fn drop(&mut self) {
        let db = self.0.db.to_str().unwrap();
        let _ = self
            .0
            .cli(&["serve", "stop", "--force", "--timeout", "5", "--db", db]);
    }
}

/// T1.11 — `serve stop` / `serve start` restores both an in-flight tool turn and
/// an unanswered permission ask cleanly, plus rejects URI-form `--db` arguments.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_stop_start_restore_and_db_uri_rejection() {
    let mut env = E2eEnvBuilder::new()
        .yolo(false)
        .permission_model("default")
        .build()
        .await
        .unwrap();

    // Session A (yolo): starts an in-flight bash tool that creates restart-started.txt.
    let session_a = env.create_session().await.unwrap();
    env.patch_json(
        &format!("/v2/sessions/{session_a}"),
        &json!({ "permissionMode": "yolo" }),
    )
    .await
    .unwrap();
    env.fake
        .push_scripts([tool_step(
            "bash",
            json!({"command": "printf started > restart-started.txt; sleep 600; printf finished > restart-finished.txt"}),
        )])
        .unwrap();
    env.client
        .create_turn_request(&pb::CreateTurnRequest {
            session: session_a.to_string(),
            kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                text: "run long bash".into(),
                ..Default::default()
            })),
        })
        .await
        .unwrap();
    wait_until("tool starts", Duration::from_secs(30), || async {
        Ok(env.project_path("restart-started.txt").exists())
    })
    .await
    .unwrap();

    // Session B (manual): blocks on an unanswered bash permission ask.
    let session_b = env.create_session().await.unwrap();
    env.fake
        .push_scripts([tool_step(
            "bash",
            json!({"command": "printf leaked > restart-perm-started.txt; sleep 600"}),
        )])
        .unwrap();
    env.client
        .create_turn_request(&pb::CreateTurnRequest {
            session: session_b.to_string(),
            kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                text: "ask bash permission".into(),
                ..Default::default()
            })),
        })
        .await
        .unwrap();
    let permission = env
        .wait_permission_id(Duration::from_secs(30))
        .await
        .unwrap();

    // Queue restore completions for both sessions, stop foreground serve, start daemon.
    env.fake
        .push_scripts([
            text_step("RESTORE_COMPLETED"),
            text_step("RESTORE_COMPLETED"),
        ])
        .unwrap();
    let db = env.backend.db.to_str().unwrap().to_string();
    let stopped = env.backend.cli(&["serve", "stop", "--db", &db]).unwrap();
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(env.backend.shutdown().unwrap().success());

    let _daemon = RestartDaemon(&env.backend);
    let args = [
        "serve",
        "start",
        "--db",
        &db,
        "--model",
        "fake/model",
        "--json",
    ];
    let started = env.backend.cli(&args).unwrap();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let ready: Value = serde_json::from_slice(&started.stdout).unwrap();
    let url = ready["url"].as_str().unwrap();
    let client = hya_sdk::Client::new(url);

    for (label, session) in [
        ("in-flight tool", session_a),
        ("unanswered permission", session_b),
    ] {
        wait_until(
            &format!("{label} restored turn completes"),
            Duration::from_secs(30),
            || async {
                Ok(load_raw_events(&client, session, None)
                    .await
                    .unwrap()
                    .iter()
                    .any(|e| {
                        matches!(
                            &e.event,
                            Event::TextDelta { delta, .. } | Event::TextReplace { text: delta, .. }
                                if delta.contains("RESTORE_COMPLETED")
                        )
                    }))
            },
        )
        .await
        .unwrap_or_else(|error| {
            panic!(
                "{label}: {error}; daemon log={}",
                std::fs::read_to_string(format!("{db}.server.log")).unwrap_or_default()
            )
        });

        let events = load_raw_events(&client, session, None).await.unwrap();
        assert!(events.iter().any(|e| matches!(
            e.event,
            Event::MessageFinished {
                role: Role::Assistant,
                cause: Some(FinishCause::Shutdown),
                ..
            }
        )));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.event, Event::ToolResult { .. }))
        );
        let restores: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.event {
                Event::TextDelta { delta, .. }
                    if delta.contains("Your previous turn was interrupted by a restart.") =>
                {
                    Some(delta)
                }
                _ => None,
            })
            .collect();
        assert_eq!(restores.len(), 1, "{label}");
        assert!(restores[0].contains("bash (call "), "{label}");
        assert!(env.fake_requests().unwrap().iter().any(|request| {
            request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| {
                    message["role"] == "user"
                        && message["content"].as_str() == Some(restores[0].as_str())
                })
        }));
    }

    assert!(!env.project_path("restart-finished.txt").exists());
    assert!(!env.project_path("restart-perm-started.txt").exists());
    assert!(client.list_interactions(None).await.unwrap().is_empty());
    let late = env
        .http
        .post(format!("{url}/v2/interactions/{permission}/respond"))
        .json(&json!({"permission": {"allowed": true}}))
        .send()
        .await
        .unwrap();
    assert_eq!(late.status(), reqwest::StatusCode::NOT_FOUND);

    // Starting an already running daemon must not inject a second restore.
    let again = env.backend.cli(&args).unwrap();
    assert!(again.status.success());
    assert_eq!(
        load_raw_events(&client, session_a, None)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(
                &e.event,
                Event::TextDelta { delta, .. }
                    if delta.contains("Your previous turn was interrupted by a restart.")
            ))
            .count(),
        1
    );

    // Invalid URI-form `--db` values are rejected before creating any files.
    let dir = unique_temp_dir("hya-db-arguments").unwrap();
    let commands: &[&[&str]] = &[
        &["serve", "status"],
        &["serve", "start"],
        &["serve"],
        &["exec", "hello"],
        &["workflow", "list"],
        &["sessions"],
        &["tail-session", "invalid-session"],
        &[],
    ];
    for uri in ["sqlite::memory:", "file:events.db?mode=memory"] {
        for command in commands {
            let output = std::process::Command::new(default_backend_bin())
                .args(["--db", uri])
                .args(*command)
                .current_dir(&dir)
                .env("HOME", &dir)
                .env("XDG_CONFIG_HOME", &dir)
                .output()
                .unwrap();
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success() && error.contains("plain filesystem path"),
                "{command:?} {uri}: {:?} {error}",
                output.status.code()
            );
        }
    }
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

async fn run_bare(env: &HeadlessEnv, db: Option<&Path>, json: bool, prompt: &str) -> Output {
    let mut cmd = env.command();
    cmd.args(["--model", "fake/model"]);
    if let Some(db) = db {
        cmd.arg("--db").arg(db);
    }
    cmd.args(["exec", "--bare"]);
    if json {
        cmd.arg("--json");
    }
    cmd.arg(prompt);
    let output = tokio::time::timeout(Duration::from_secs(20), cmd.output())
        .await
        .expect("bare finished")
        .expect("spawn bare");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// T1.15 (bare kernel) — `exec --bare` writes todos, triggers the completion check,
/// streams valid JSONL matching `tail-session`, supports in-memory mode, and reads stdin `-`.
#[tokio::test]
async fn bare_exec_todo_stdin_json_and_tail_session() {
    let env = E2eEnvBuilder::new()
        .build_headless()
        .await
        .expect("headless env");

    // 1. Persisted `--bare --json` + todo + completion check + `tail-session` parity.
    env.fake
        .push_scripts([
            tool_step(
                "todo",
                json!({"ops": [{"op": "add", "content": "Verify the bare run"}]}),
            ),
            text_step("draft"),
            text_step("BARE_TODO_WRITTEN"),
        ])
        .unwrap();
    let db = env.project.join("bare.db");
    let output = run_bare(&env, Some(&db), true, "track a completion criterion").await;
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    assert!(stdout.ends_with('\n'));
    let envelopes: Vec<Envelope> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("JSONL envelope"))
        .collect();
    let Event::SessionCreated { session, .. } = envelopes.first().unwrap().event else {
        panic!("first event must create the session");
    };
    for (index, envelope) in envelopes.iter().enumerate() {
        assert_eq!(envelope.seq.0, index as u64 + 1);
        assert_eq!(envelope.event.session(), Some(session));
    }
    let todo_call = envelopes
        .iter()
        .find_map(|e| match &e.event {
            Event::ToolCallRequested { call, name, .. } if name.as_str() == "todo" => Some(*call),
            _ => None,
        })
        .expect("todo call recorded");
    assert!(envelopes.iter().any(|e| matches!(
        &e.event,
        Event::ToolResult { call, output, .. }
            if *call == todo_call
                && output == "Current todos:\n- #1 [pending] Verify the bare run"
    )));
    assert!(envelopes.iter().any(|e| matches!(
        &e.event,
        Event::TextReplace { text, .. } if text == "BARE_TODO_WRITTEN"
    )));
    assert!(matches!(
        envelopes.last().unwrap().event,
        Event::MessageFinished {
            role: Role::Assistant,
            ..
        }
    ));

    let requests = env.fake.requests().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|msg| {
                msg["role"] == "tool"
                    && msg["content"] == "Current todos:\n- #1 [pending] Verify the bare run"
            })
    );
    assert!(
        requests[2]["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .is_some_and(|text| text.starts_with("[hya] Completion check"))
    );

    // `tail-session` matches `--json` byte-for-byte.
    let mut tail = env.command();
    tail.arg("--db")
        .arg(&db)
        .arg("tail-session")
        .arg(session.to_string());
    let replay = tokio::time::timeout(Duration::from_secs(20), tail.output())
        .await
        .expect("tail finished")
        .expect("spawn tail");
    assert!(replay.status.success());
    assert_eq!(output.stdout, replay.stdout);

    // 2. In-memory `--bare --json` (no `--db`).
    env.fake
        .push_scripts([text_step("IN_MEMORY_BARE_OK")])
        .unwrap();
    let mem_out = run_bare(&env, None, true, "in memory bare").await;
    let mem_str = std::str::from_utf8(&mem_out.stdout).unwrap();
    assert!(mem_str.contains("IN_MEMORY_BARE_OK"));

    // 3. Stdin `-` preserves UTF-8 and whitespace verbatim.
    let reqs_before = env.fake.requests().unwrap().len();
    env.fake
        .push_scripts([text_step("STDIN_PROMPT_RECEIVED")])
        .unwrap();
    let prompt = "  Summarize café notes.\nKeep the original spacing.\n";
    let input = env.root.join("prompt.txt");
    std::fs::write(&input, format!("{prompt}\n")).unwrap();
    let mut stdin_cmd = env.command();
    stdin_cmd
        .args(["--model", "fake/model", "--db"])
        .arg(env.root.join("stdin.db"))
        .args(["exec", "--bare", "-"])
        .stdin(std::fs::File::open(&input).unwrap());
    let stdin_out = tokio::time::timeout(Duration::from_secs(20), stdin_cmd.output())
        .await
        .expect("stdin bare finished")
        .expect("spawn stdin bare");
    assert!(
        stdin_out.status.success(),
        "{}",
        String::from_utf8_lossy(&stdin_out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&stdin_out.stdout).trim(),
        "STDIN_PROMPT_RECEIVED"
    );
    let stdin_req = &env.fake.requests().unwrap()[reqs_before];
    let user_msg = stdin_req["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|msg| msg["role"] == "user")
        .expect("user message");
    assert_eq!(user_msg["content"], prompt);
}
