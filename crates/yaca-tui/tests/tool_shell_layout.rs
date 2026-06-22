#![allow(dead_code)]

mod render_support;

use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::AppState;

use render_support::render;

#[test]
fn completed_shell_tool_shows_exit_code_when_command_fails() {
    // Given: a completed shell tool whose structured output includes a failing exit code.
    let mut state = AppState::default();
    with_completed_shell_failure(&mut state);

    // When: the transcript is rendered.
    let output = render(&mut state, 100, 24);

    // Then: the compact status row exposes the exit code while stderr stays readable.
    assert!(
        output.contains("→ Shell false"),
        "shell action row should include the command summary:\n{output}"
    );
    assert!(
        output.contains("exit 2 ✗ 9ms"),
        "shell failure status should include the process exit code:\n{output}"
    );
    assert!(
        !output.contains("completed (exit 2)"),
        "shell failure row should not describe a non-zero exit as completed:\n{output}"
    );
    assert!(
        output.contains("▏ boom"),
        "stderr should still render as a railed output block:\n{output}"
    );
    assert!(
        !output.contains("exit_code"),
        "exit_code metadata should not leak as raw JSON:\n{output}"
    );
}

#[test]
fn completed_shell_tool_strips_ansi_escape_sequences_like_opencode() {
    // Given: shell stderr includes ANSI color escapes.
    let mut state = AppState::default();
    with_completed_shell_ansi_output(&mut state);

    // When: the transcript is rendered.
    let output = render(&mut state, 100, 24);

    // Then: the readable output remains, but terminal escape sequences do not leak.
    assert!(
        output.contains("▏ red text"),
        "shell output should preserve readable text after ANSI stripping:\n{output}"
    );
    assert!(
        !output.contains("[31m") && !output.contains("[0m"),
        "shell output should strip ANSI control sequences:\n{output}"
    );
    assert!(
        !output.contains("ignored") && !output.contains("Bred"),
        "shell output should strip broader ANSI escape families:\n{output}"
    );
}

fn with_completed_shell_failure(state: &mut AppState) {
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
            input: json!({ "command": "false" }),
        },
    ));
    state.apply(&env(
        4,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output: json!({ "stderr": "boom", "exit_code": 2 }),
            time_ms: 9,
        },
    ));
}

fn with_completed_shell_ansi_output(state: &mut AppState) {
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
            input: json!({ "command": "printf colors" }),
        },
    ));
    state.apply(&env(
        4,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output: json!({
                "stderr": "\u{1b}[31m\u{1b}(Bred \u{1b}Pignored\u{1b}\\text\u{1b}[0m",
                "exit_code": 1
            }),
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
