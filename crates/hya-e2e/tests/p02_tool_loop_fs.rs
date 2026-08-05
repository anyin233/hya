//! T1.3–T1.5 — multi-round tool loop: write → read → shell.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use hya_proto::Event;
use serde_json::json;

#[tokio::test]
async fn t1_3_to_t1_5_write_read_shell_via_fake_tool_calls() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step(
                "write",
                json!({
                    "filePath": "e2e-note.txt",
                    "content": "hya-e2e-fs"
                }),
            ),
            tool_step(
                "read",
                json!({
                    "filePath": "e2e-note.txt"
                }),
            ),
            tool_step(
                "shell",
                json!({
                    "command": "printf shell-ok > e2e-shell.txt"
                }),
            ),
            text_step("TOOLS_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let resp = env
        .prompt(session, "write note, read it, shell")
        .await
        .expect("multi-round prompt");
    let _ = resp;

    assert_eq!(
        env.read_project_file("e2e-note.txt").expect("note file"),
        "hya-e2e-fs"
    );
    assert_eq!(
        env.read_project_file("e2e-shell.txt").expect("shell file"),
        "shell-ok"
    );

    let events = env.events(session, None).await.expect("events");
    let mut saw_done = false;
    let mut text_bits = String::new();
    for env_evt in events {
        match env_evt.event {
            Event::TextDelta { delta, .. } => {
                if delta.contains("TOOLS_DONE") {
                    saw_done = true;
                }
                text_bits.push_str(&delta);
            }
            Event::TextReplace { text, .. } => {
                if text.contains("TOOLS_DONE") {
                    saw_done = true;
                }
                text_bits.push_str(&text);
            }
            _ => {}
        }
    }
    let fake_n = env.fake.requests().unwrap().len();
    // Disk side effects are the primary oracle for tool execution.
    assert!(
        fake_n >= 3,
        "fake llm should receive write/read/shell turns, got {fake_n}; text={text_bits:?}"
    );
    assert!(
        saw_done || text_bits.contains("TOOLS_DONE"),
        "final assistant text TOOLS_DONE expected; fake_n={fake_n}; text={text_bits:?}"
    );
}
