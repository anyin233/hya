//! T1.15 — todowrite plane + edit tool side effects (basic agent tools beyond shell/fs write).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::json;

#[tokio::test]
async fn t1_15_todowrite_visible_on_session_todo_route() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step(
                "todowrite",
                json!({
                    "todos": [
                        {
                            "content": "E2E_TODO_ITEM",
                            "status": "pending",
                            "priority": "high"
                        }
                    ]
                }),
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
        blob.contains("E2E_TODO_ITEM"),
        "GET /session/{{id}}/todo must list written item; todos={todos}; {}",
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
                    "filePath": "edit-me.txt",
                    "oldString": "OLD_TOKEN",
                    "newString": "NEW_TOKEN"
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
