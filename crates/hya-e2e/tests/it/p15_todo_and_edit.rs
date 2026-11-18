//! T1.15 — kernel todo tool + edit tool side effects (basic agent tools beyond shell/fs write).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::json;

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

#[tokio::test]
async fn t1_15_bare_exec_can_write_todos() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step(
                "todo",
                json!({"ops": [{"op": "add", "content": "Verify the bare run"}]}),
            ),
            text_step("BARE_TODO_WRITTEN"),
        ])
        .build()
        .await
        .expect("e2e env");
    let backend = &env.backend;
    let root = backend.project.parent().expect("e2e root");
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio::process::Command::new(&backend.binary)
            .args(["--model", "fake/model", "--db"])
            .arg(root.join("bare.db"))
            .args(["exec", "--bare", "track a completion criterion"])
            .current_dir(&backend.project)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", &backend.xdg_config_home)
            .env("XDG_DATA_HOME", &backend.xdg_data_home)
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env_remove("HYA_MODEL")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("bare execution finished")
    .expect("spawn hya exec --bare");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "BARE_TODO_WRITTEN"
    );
    let requests = env.fake.requests().unwrap();
    assert_eq!(requests.len(), 2);
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
