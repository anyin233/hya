#![allow(dead_code, clippy::field_reassign_with_default)]

mod render_support;

use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::AppState;

use render_support::render;

#[test]
fn completed_tool_stdout_renders_as_a_readable_output_block() {
    // Given: an assistant turn with a completed shell tool that produced stdout.
    let mut state = AppState::default();
    with_completed_shell_output(&mut state, "line one\nline two");

    // When: the transcript is rendered in a normal terminal width.
    let output = render(&mut state, 100, 24);

    // Then: the tool status remains compact, and stdout is shown as a railed block below it.
    let rows: Vec<&str> = output.lines().collect();
    let Some(status_row) = rows.iter().position(|row| row.contains("→ Shell printf")) else {
        panic!("completed shell status row missing:\n{output}");
    };
    let Some(first_output_row) = rows.iter().position(|row| row.contains("▏ line one")) else {
        panic!("first stdout row missing:\n{output}");
    };
    let Some(second_output_row) = rows.iter().position(|row| row.contains("▏ line two")) else {
        panic!("second stdout row missing:\n{output}");
    };

    assert!(
        status_row < first_output_row && first_output_row < second_output_row,
        "stdout block should render after the compact tool status row:\n{output}"
    );
}

#[test]
fn compact_tool_status_keeps_duration_on_the_status_row_at_eighty_columns() {
    // Given: a completed shell tool whose input is long enough to pressure an 80-column layout.
    let mut state = AppState::default();
    with_completed_shell_output(&mut state, "line one\nline two");

    // When: the transcript is rendered at the narrow supported width.
    let output = render(&mut state, 80, 24);

    // Then: the duration stays on the compact tool status row instead of wrapping alone.
    let rows: Vec<&str> = output.lines().collect();
    assert!(
        rows.iter()
            .any(|row| row.contains("→ Shell") && row.contains("completed ✓ 9ms")),
        "completed duration should stay attached to its status row:\n{output}"
    );
    assert!(
        rows.iter().all(|row| row.trim() != "9ms"),
        "duration should not render as a standalone wrapped line:\n{output}"
    );
    assert!(
        !output.contains("printf line one && printf line two"),
        "long shell inputs should be summarized before they pressure the status row:\n{output}"
    );
}

fn with_completed_shell_output(state: &mut AppState, output: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new("shell");
    state.apply(&env(
        1,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
        },
    ));
    state.apply(&env(
        2,
        Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name: name.clone(),
        },
    ));
    state.apply(&env(
        3,
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name,
            input: json!({ "cmd": "printf 'line one\\nline two'" }),
        },
    ));
    state.apply(&env(
        4,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output: json!({ "stdout": output }),
            time_ms: 9,
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
