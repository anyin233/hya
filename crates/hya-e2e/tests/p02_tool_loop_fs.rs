//! T1.3–T1.5 — multi-round tool loop: write → read → bash.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use hya_proto::Event;
use serde_json::json;

#[tokio::test]
async fn t1_3_to_t1_5_write_read_bash_via_fake_tool_calls() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step(
                "write",
                json!({
                    "path": "e2e-note.txt",
                    "content": "hya-e2e-fs"
                }),
            ),
            tool_step(
                "read",
                json!({
                    "filePath": "e2e-note.txt",
                    "path": "e2e-note.txt",
                    "offset": 0,
                    "limit": 2000
                }),
            ),
            tool_step(
                "bash",
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
        .prompt(session, "write note, read it, bash")
        .await
        .expect("multi-round prompt");
    let _ = resp;

    assert_eq!(
        env.read_project_file("e2e-note.txt").expect("note file"),
        "hya-e2e-fs"
    );
    assert_eq!(
        env.read_project_file("e2e-shell.txt").expect("bash file"),
        "shell-ok"
    );

    let events = env.events(session, None).await.expect("events");
    let expected_read_input = json!({
        "filePath": "e2e-note.txt",
        "path": "e2e-note.txt",
        "offset": 0,
        "limit": 2000
    });
    let correlated_read_result = events.iter().any(|result| {
        let Event::ToolResult {
            call: result_call, ..
        } = &result.event
        else {
            return false;
        };
        events.iter().any(|requested| {
            matches!(
                &requested.event,
                Event::ToolCallRequested {
                    call: requested_call,
                    name,
                    input,
                    ..
                } if requested_call == result_call
                    && name.as_str() == "read"
                    && input == &expected_read_input
            )
        })
    });
    assert!(
        correlated_read_result,
        "expected the captured Read call to complete with a correlated ToolResult; events={events:?}"
    );
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
        "fake llm should receive write/read/bash turns, got {fake_n}; text={text_bits:?}"
    );
    assert!(
        saw_done || text_bits.contains("TOOLS_DONE"),
        "final assistant text TOOLS_DONE expected; fake_n={fake_n}; text={text_bits:?}"
    );
}
