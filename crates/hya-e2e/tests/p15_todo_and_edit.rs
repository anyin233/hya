//! T1.15 — todo__ tool group + edit tool side effects (basic agent tools beyond shell/fs write).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::json;

#[tokio::test]
async fn t1_15_todo_group_visible_on_session_todo_route() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step(
                "todo__update_content",
                json!({
                    "operations": [
                        { "op": "add", "content": "E2E_TODO_ITEM" },
                        { "op": "add", "content": "E2E_SECOND_ITEM" }
                    ]
                }),
            ),
            tool_step(
                "todo__update_status",
                json!({ "updates": [{ "id": "1", "status": "in_progress" }] }),
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
        "stable plane ids must reach the wire; todos={todos}; {}",
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
