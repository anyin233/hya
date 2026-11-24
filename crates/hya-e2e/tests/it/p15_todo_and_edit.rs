//! T1.15 — kernel todo tool + edit tool side effects (basic agent tools beyond shell/fs write).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnv, E2eEnvBuilder, text_step, tool_step};
use hya_proto::{Envelope, Event, Role};
use serde_json::json;
use std::{path::Path, process::Output, time::Duration};
use tokio::process::Command;

#[tokio::test]
async fn t1_15_todo_group_visible_on_session_todo_route() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
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
            text_step("TODO_WRITTEN"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "write a todo")
        .await
        .expect("todo prompt");

    let todos = env.session_todos(&session).await.expect("todo list");
    let blob = todos.to_string();
    assert!(
        blob.contains("E2E_TODO_ITEM") && blob.contains("E2E_SECOND_ITEM"),
        "GET /session/{{id}}/todo must list written items; todos={todos}; {}",
        env.diagnostics()
    );
    assert!(
        blob.contains("IN_PROGRESS") || blob.contains("in_progress"),
        "status update must reach the todo projection; todos={todos}; {}",
        env.diagnostics()
    );
    assert!(
        blob.contains("\"id\":\"1\"") || blob.contains("\"id\": \"1\""),
        "todo ids must reach the wire; todos={todos}; {}",
        env.diagnostics()
    );
}

#[tokio::test]
async fn t1_15_edit_tool_mutates_existing_file() {
    let env = E2eEnvBuilder::new()
        .project_file("edit-me.txt", b"hello OLD_TOKEN world\n".to_vec())
        .scripts(vec![
            tool_step(
                "edit",
                json!({
                    "path": "edit-me.txt",
                    "edits": [{
                        "op": "replace_text",
                        "oldText": "OLD_TOKEN",
                        "newText": "NEW_TOKEN"
                    }]
                }),
            ),
            text_step("EDIT_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "edit the file")
        .await
        .expect("edit prompt");

    let body = env.read_project_file("edit-me.txt").expect("edited file");
    assert!(
        body.contains("NEW_TOKEN"),
        "edit must replace token; body={body:?}; {}",
        env.diagnostics()
    );
    assert!(
        !body.contains("OLD_TOKEN"),
        "old token must be gone; body={body:?}; {}",
        env.diagnostics()
    );
}

async fn bare_todo_env() -> E2eEnv {
    E2eEnvBuilder::new()
        .scripts(vec![
            tool_step(
                "todo",
                json!({"ops": [{"op": "add", "content": "Verify the bare run"}]}),
            ),
            // The first answer after a tool call gets the completion check; the next one ends the run.
            text_step("draft"),
            text_step("BARE_TODO_WRITTEN"),
        ])
        .build()
        .await
        .expect("e2e env")
}

fn bare_command(env: &E2eEnv) -> Command {
    let backend = &env.backend;
    let root = backend.project.parent().expect("e2e root");
    let mut command = Command::new(&backend.binary);
    command
        .current_dir(&backend.project)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", &backend.xdg_config_home)
        .env("XDG_DATA_HOME", &backend.xdg_data_home)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env_remove("HYA_MODEL")
        .kill_on_drop(true);
    command
}

async fn successful_output(mut command: Command) -> Output {
    let output = tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .expect("hya finished")
        .expect("spawn hya");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

async fn bare_todo_output(env: &E2eEnv, db: Option<&Path>, json: bool) -> Output {
    let mut command = bare_command(env);
    command.args(["--model", "fake/model"]);
    if let Some(db) = db {
        command.arg("--db").arg(db);
    }
    command.args(["exec", "--bare"]);
    if json {
        command.arg("--json");
    }
    command.arg("track a completion criterion");
    successful_output(command).await
}

#[tokio::test]
async fn t1_15_bare_exec_can_write_todos() {
    let env = bare_todo_env().await;
    let db = env.backend.project.join("bare.db");
    let output = bare_todo_output(&env, Some(&db), false).await;
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "BARE_TODO_WRITTEN"
    );
    let requests = env.fake.requests().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .is_some_and(|text| text.starts_with("[hya] Completion check")),
        "the completion check must be the last message of the request after the first answer"
    );
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
}

#[tokio::test]
async fn t1_15_bare_exec_reads_prompt_from_stdin() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![text_step("STDIN_PROMPT_RECEIVED")])
        .build()
        .await
        .expect("e2e env");
    let backend = &env.backend;
    let root = backend.project.parent().expect("e2e root");
    let prompt = "  Summarize café notes.\nKeep the original spacing.\n";
    let input = root.join("prompt.txt");
    std::fs::write(&input, format!("{prompt}\n")).expect("write stdin fixture");
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio::process::Command::new(&backend.binary)
            .args(["--model", "fake/model", "--db"])
            .arg(root.join("bare.db"))
            .args(["exec", "--bare", "-"])
            .current_dir(&backend.project)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", &backend.xdg_config_home)
            .env("XDG_DATA_HOME", &backend.xdg_data_home)
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env_remove("HYA_MODEL")
            .stdin(std::fs::File::open(&input).expect("open stdin fixture"))
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("bare execution finished")
    .expect("spawn hya exec --bare -");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "STDIN_PROMPT_RECEIVED"
    );
    let requests = env.fake.requests().unwrap();
    let user_message = requests[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|msg| msg["role"] == "user")
        .expect("provider request contains a user message");
    assert_eq!(user_message["content"], prompt);
}

#[tokio::test]
async fn t1_15_bare_json_streams_in_memory_session() {
    let env = bare_todo_env().await;
    let output = bare_todo_output(&env, None, true).await;
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    assert!(stdout.ends_with('\n'));
    let envelopes: Vec<Envelope> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("stdout contains only event envelopes"))
        .collect();
    let Event::SessionCreated { session, .. } = envelopes.first().unwrap().event else {
        panic!("first event must create the session");
    };
    for (index, envelope) in envelopes.iter().enumerate() {
        assert_eq!(envelope.seq.0, index as u64 + 1);
        assert_eq!(envelope.event.session(), Some(session));
    }
    assert!(envelopes.iter().any(|envelope| matches!(
        &envelope.event, Event::TextDelta { delta, .. } if delta == "track a completion criterion"
    )));
    let call = envelopes
        .iter()
        .find_map(|envelope| match &envelope.event {
            Event::ToolCallRequested { call, name, .. } if name.as_str() == "todo" => Some(*call),
            _ => None,
        })
        .expect("todo call recorded");
    assert!(envelopes.iter().any(|envelope| matches!(
        &envelope.event, Event::ToolResult { call: result_call, output, .. }
            if *result_call == call && output == "Current todos:\n- #1 [pending] Verify the bare run"
    )));
    assert!(envelopes.iter().any(|envelope| matches!(
        &envelope.event, Event::TextReplace { text, .. } if text == "BARE_TODO_WRITTEN"
    )));
    assert!(matches!(
        envelopes.last().unwrap().event,
        Event::MessageFinished {
            role: Role::Assistant,
            ..
        }
    ));
}

#[tokio::test]
async fn t1_15_bare_json_matches_tail_session() {
    let env = bare_todo_env().await;
    let db = env.backend.project.join("bare-json.db");
    let output = bare_todo_output(&env, Some(&db), true).await;
    let first = output.stdout.split(|&byte| byte == b'\n').next().unwrap();
    let first: Envelope = serde_json::from_slice(first).unwrap();
    let session = first.event.session().unwrap();

    let mut tail = bare_command(&env);
    tail.arg("--db")
        .arg(&db)
        .arg("tail-session")
        .arg(session.to_string());
    let replay = successful_output(tail).await;
    assert_eq!(output.stdout, replay.stdout);
}
