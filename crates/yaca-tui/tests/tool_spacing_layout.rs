#![allow(dead_code)]

mod render_support;

use render_support::render;
use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::AppState;

#[test]
fn completed_multiline_tool_output_inserts_one_spacer_before_next_tool() {
    // Given: a completed multiline shell output block is followed by another tool row.
    let mut state = AppState::default();
    with_completed_shell_then_completed_glob(&mut state);

    // When: the transcript renders in the OpenCode-style streaming area.
    let output = render(&mut state, 100, 28);

    // Then: exactly one blank row separates the output block from the next tool.
    let rows: Vec<&str> = output.lines().collect();
    let Some(output_row) = rows.iter().position(|row| row.contains("▏ total 4")) else {
        panic!("last shell output row missing:\n{output}");
    };
    let Some(glob_row) = rows.iter().position(|row| row.contains("✱ Glob")) else {
        panic!("following glob row missing:\n{output}");
    };
    assert_eq!(
        glob_row,
        output_row + 2,
        "OpenCode keeps one blank row between multiline tool output and the next tool:\n{output}"
    );
    assert!(
        rows[output_row + 1].trim().is_empty(),
        "separator row should be blank:\n{output}"
    );
}

fn with_completed_shell_then_completed_glob(state: &mut AppState) {
    let session = SessionId::new();
    let message = MessageId::new();
    state.apply(&env(
        1,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
        },
    ));

    complete_tool(
        state,
        session,
        message,
        2,
        "shell",
        json!({ "cmd": "ls -la" }),
        json!({ "stdout": "demo.ts\ntotal 4" }),
    );
    complete_tool(
        state,
        session,
        message,
        5,
        "glob",
        json!({ "pattern": "**/*tool*", "path": "src/cli/cmd" }),
        json!({ "count": 1 }),
    );
}

fn complete_tool(
    state: &mut AppState,
    session: SessionId,
    message: MessageId,
    base_seq: u64,
    tool_name: &str,
    input: serde_json::Value,
    output: serde_json::Value,
) {
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new(tool_name);
    state.apply(&env(
        base_seq,
        Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name: name.clone(),
        },
    ));
    state.apply(&env(
        base_seq + 1,
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name,
            input,
        },
    ));
    state.apply(&env(
        base_seq + 2,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output,
            time_ms: 4,
        },
    ));
}

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}
